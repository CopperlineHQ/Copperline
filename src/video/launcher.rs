// SPDX-License-Identifier: GPL-3.0-or-later

//! The pre-boot machine-configuration screen's data model.
//!
//! When Copperline is started with no config (and from the "Machine
//! Configuration..." menu item) a launcher panel lets the user pick a machine
//! and configure everything about it before pressing Run. This module holds the
//! editable model behind that panel; the panel's pixel layout and hit-testing
//! live in [`crate::video::ui`], and the App integration (file dialogs, Run,
//! Save) lives in [`crate::video::window`].
//!
//! [`MachineSetup`] is a fully-typed editable mirror of the configurable
//! machine. It is built from, and converted back to, the loadable
//! [`RawConfig`]: loading parses a file into a `RawConfig`, validates it through
//! the existing `TryFrom<RawConfig> for Config` pipeline, then fills the typed
//! fields; Run and Save go the other way via [`MachineSetup::to_raw`], so the
//! configuration screen reuses all of the config layer's validation and
//! profile-default logic instead of duplicating it. `to_raw` emits only the
//! fields that differ from the selected profile's defaults, so a saved file
//! reads like the hand-written `*.example.toml`.

use crate::bus::PortDevice;
use crate::chipset::agnus::{AgnusRevision, VideoStandard};
use crate::chipset::denise::DeniseRevision;
use crate::config::{
    format_size, machine_profile_defaults, AudioFilterMode, BezelStyle, BridgeCable, BridgeDensity,
    BridgeDriver, BridgeReadMode, ChannelMode, Chipset, Config, CpuModel, DisplayScaling,
    FluxBridgeConfig, JoystickInputMode, MachineModel, MenuScale, MouseCapture, Mt32Lcd, Overscan,
    PacingBudget, ParallelDevice, PixelAspect, RawConfig, RawDrive, RawFilesysMount,
    RawFloppyDrive, RawHostDisk, RawZorroBoard, RtgCard, ScsiController, SerialMode, ShaderMode,
    Tint, WarpSpeed, BOOT_PRI_NEVER,
};
use crate::ide_zorro::LidePersonality;
use crate::memory::{RamInit, DEFAULT_RAM_PATTERN, DEFAULT_RANDOM_RAM_SEED};
use crate::net::NetConfig;
use crate::zorro::{ConfigOption, ConfigOptionKind, LoadedZorroBoard};
use anyhow::Result;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A Zorro board entry in the launcher: its metadata-file path, the config
/// option schema parsed from that manifest (empty for RAM boards or on load
/// error), and the user's per-board setting overrides (layered over the
/// manifest defaults). Editing in the config panel mutates `overrides`.
#[derive(Debug, Clone)]
pub struct ZorroBoardSetup {
    metadata: PathBuf,
    options: Vec<ConfigOption>,
    /// Effective manifest defaults (option defaults overlaid by `[config]`,
    /// file paths resolved), the baseline the user's overrides layer over.
    defaults: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
}

impl ZorroBoardSetup {
    /// Load a board's option schema + defaults from its manifest. RAM boards
    /// and load failures yield an entry with no editable options.
    fn load(metadata: PathBuf) -> Self {
        let (options, defaults) = match crate::zorro::load_board_metadata(&metadata) {
            Ok(LoadedZorroBoard::Wasm {
                options,
                default_config,
                ..
            }) => (options, default_config),
            _ => (Vec::new(), BTreeMap::new()),
        };
        Self {
            metadata,
            options,
            defaults,
            overrides: BTreeMap::new(),
        }
    }

    /// File name (or full path) for display.
    pub fn name(&self) -> String {
        self.metadata
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.metadata.display().to_string())
    }

    pub fn options(&self) -> &[ConfigOption] {
        &self.options
    }

    /// The current value of option `opt`: the user's override, else the
    /// effective manifest default, else empty.
    pub fn value(&self, opt: usize) -> String {
        let Some(o) = self.options.get(opt) else {
            return String::new();
        };
        self.overrides
            .get(&o.key)
            .or_else(|| self.defaults.get(&o.key))
            .cloned()
            .unwrap_or_default()
    }

    fn set(&mut self, opt: usize, value: String) {
        if let Some(o) = self.options.get(opt) {
            self.overrides.insert(o.key.clone(), value);
        }
    }

    /// Drop the override, reverting the option to its manifest default.
    fn clear(&mut self, opt: usize) {
        if let Some(o) = self.options.get(opt) {
            self.overrides.remove(&o.key);
        }
    }

    /// Step an enum/int option by one (forward or back).
    fn cycle(&mut self, opt: usize, forward: bool) {
        let Some(o) = self.options.get(opt) else {
            return;
        };
        let next = match &o.kind {
            ConfigOptionKind::Enum(choices) if !choices.is_empty() => {
                let cur = self.value(opt);
                let idx = choices.iter().position(|c| *c == cur).unwrap_or(0);
                let n = choices.len();
                let idx = if forward {
                    (idx + 1) % n
                } else {
                    (idx + n - 1) % n
                };
                choices[idx].clone()
            }
            ConfigOptionKind::Int => {
                let cur: i64 = self.value(opt).trim().parse().unwrap_or(0);
                let next = if forward { cur + 1 } else { cur - 1 };
                next.to_string()
            }
            _ => return,
        };
        self.set(opt, next);
    }

    /// Flip a bool option.
    fn toggle(&mut self, opt: usize) {
        if matches!(
            self.options.get(opt).map(|o| &o.kind),
            Some(ConfigOptionKind::Bool)
        ) {
            let on = self.value(opt).trim().eq_ignore_ascii_case("true");
            self.set(opt, (!on).to_string());
        }
    }

    /// The TOML override value for an option, typed per its kind, or `None`
    /// when the user has left it at the manifest default.
    fn override_toml(&self, o: &ConfigOption) -> Option<toml::Value> {
        let raw = self.overrides.get(&o.key)?;
        Some(match o.kind {
            ConfigOptionKind::Bool => toml::Value::Boolean(raw.trim().eq_ignore_ascii_case("true")),
            ConfigOptionKind::Int => raw
                .trim()
                .parse::<i64>()
                .map(toml::Value::Integer)
                .unwrap_or_else(|_| toml::Value::String(raw.clone())),
            _ => toml::Value::String(raw.clone()),
        })
    }
}

/// The configuration screen's category tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherTab {
    System,
    Cpu,
    Memory,
    Rom,
    Floppy,
    Storage,
    /// FluxBridge settings for one bay, reached from its Configure button.
    FluxBridge,
    BootPriority,
    HostFs,
    /// Direct WHDLoad boot (src/whdload.rs): the game to launch and what
    /// staging draws on, reached from the Storage tab.
    Whdload,
    /// The games found beside the one chosen to launch, with what the game
    /// database says about them. Only in a build with the library.
    #[cfg(feature = "game-library")]
    WhdloadLibrary,
    /// Real host storage -- an SD card, a CF card, an Amiga's own hard
    /// drive -- attached in place of a disk image. Drawn as its own layout
    /// rather than a list of settings rows, because choosing a disk is a
    /// table with a single selection, not a field.
    HostDisk,
    Cd,
    /// The `[lide]` built-in Zorro II IDE board: personality, boot ROM(s),
    /// and its drives, reached from the Storage tab.
    Lide,
    /// The "I/O Ports" strip tab, whose default category is the serial
    /// port. Parallel, networking and audio are its sibling categories,
    /// switched between via the top nav row, with no Back button --
    /// `IoPorts.label()` is therefore the strip's "I/O Ports", not
    /// "Serial Port".
    IoPorts,
    IoParallel,
    IoNetworking,
    IoAudio,
    Input,
    Zorro,
    /// The "A/V & Emu" strip tab, whose default category is Audio (its rows are
    /// the audio settings). Video and Emulation are its sibling categories,
    /// switched between via the top nav row, with no Back button.
    /// `AvAudio.label()` is therefore the strip's "A/V & Emu", not "Audio".
    AvAudio,
    AvVideo,
    AvEmulation,
    /// Where Copperline keeps what it produces and where its file dialogs
    /// open. Its rows are the configuration's `[paths]` section, saved
    /// with the rest of it and absent until one of them is set.
    AvPaths,
    /// The Create Image workshop, reached from Storage: two pages that make
    /// fresh images and touch nothing about the machine.
    CreateFloppy,
    CreateHard,
    /// The hard-disk page's geometry editor, reached from its Configure
    /// button once the geometry is set by hand.
    CreateGeometry,
}

/// Tabs shown top to bottom.
pub const TABS: &[LauncherTab] = &[
    LauncherTab::System,
    LauncherTab::Cpu,
    LauncherTab::Memory,
    LauncherTab::Rom,
    LauncherTab::Floppy,
    LauncherTab::Storage,
    // Cd, HostFs, and BootPriority are reached as sub-pages from the Storage
    // tab, so they are not top-level strip entries.
    LauncherTab::Input,
    LauncherTab::IoPorts,
    LauncherTab::Zorro,
    LauncherTab::AvAudio,
];

/// The strip with WHDLoad in it, between Zorro and A/V & Emu. This is the
/// usual one: the entry is there unless somebody has turned WHDLoad off.
#[cfg(feature = "game-library")]
const WHDLOAD_TABS: &[LauncherTab] = &[
    LauncherTab::System,
    LauncherTab::Cpu,
    LauncherTab::Memory,
    LauncherTab::Rom,
    LauncherTab::Floppy,
    LauncherTab::Storage,
    LauncherTab::Input,
    LauncherTab::IoPorts,
    LauncherTab::Zorro,
    LauncherTab::WhdloadLibrary,
    LauncherTab::AvAudio,
];

/// The left-hand strip. WHDLoad has an entry of its own unless it has been
/// turned off in A/V & Emu -> Emulation.
///
/// It lands on the Library rather than on the settings behind it: picking
/// a game is the reason to go there, and the settings are one click away
/// on the page itself.
pub fn tabs(whdload_enabled: bool) -> &'static [LauncherTab] {
    #[cfg(feature = "game-library")]
    if whdload_enabled {
        return WHDLOAD_TABS;
    }
    let _ = whdload_enabled;
    TABS
}

impl LauncherTab {
    pub fn label(self) -> &'static str {
        match self {
            LauncherTab::System => "System",
            LauncherTab::Cpu => "CPU",
            LauncherTab::Memory => "Memory",
            LauncherTab::Rom => "ROM",
            LauncherTab::Floppy => "Floppy",
            LauncherTab::FluxBridge => "FluxBridge",
            LauncherTab::Storage => "Storage",
            LauncherTab::BootPriority => "Boot Priority",
            LauncherTab::HostFs => "Host Folder",
            LauncherTab::Whdload => "WHDLoad",
            // The strip's own name for it. Inside the WHDLoad pages the
            // nav chips say Settings... and Library, from their own
            // labels, so this one is free to say which tab it is.
            #[cfg(feature = "game-library")]
            LauncherTab::WhdloadLibrary => "WHDLoad",
            LauncherTab::HostDisk => "Host Disk",
            LauncherTab::Cd => "CD",
            LauncherTab::Lide => "Lide",
            LauncherTab::IoPorts => "I/O Ports",
            LauncherTab::IoParallel => "Parallel Port",
            LauncherTab::IoNetworking => "Networking",
            LauncherTab::IoAudio => "Audio",
            LauncherTab::Input => "Input",
            LauncherTab::Zorro => "Zorro",
            LauncherTab::AvAudio => "A/V & Emu",
            LauncherTab::AvVideo => "Video",
            LauncherTab::AvEmulation => "Emulation",
            LauncherTab::AvPaths => "Paths",
            LauncherTab::CreateFloppy => "Floppy Disk",
            LauncherTab::CreateHard => "Hard Disk",
            LauncherTab::CreateGeometry => "Disk Geometry",
        }
    }

    /// The strip entry to highlight for this (possibly sub-page) tab: the Storage
    /// sub-pages keep the Storage strip entry lit, and the A/V categories keep
    /// the A/V & Emu one.
    pub fn strip_tab(self) -> LauncherTab {
        match self {
            // The settings page lights the Library entry: they are two
            // views of one thing, reached through one strip entry.
            #[cfg(feature = "game-library")]
            LauncherTab::Whdload => LauncherTab::WhdloadLibrary,
            #[cfg(not(feature = "game-library"))]
            LauncherTab::Whdload => LauncherTab::Storage,
            LauncherTab::Cd
            | LauncherTab::HostFs
            | LauncherTab::HostDisk
            | LauncherTab::BootPriority
            | LauncherTab::Lide
            | LauncherTab::CreateFloppy
            | LauncherTab::CreateHard
            | LauncherTab::CreateGeometry => LauncherTab::Storage,
            LauncherTab::FluxBridge => LauncherTab::Floppy,
            LauncherTab::AvVideo | LauncherTab::AvEmulation | LauncherTab::AvPaths => {
                LauncherTab::AvAudio
            }
            LauncherTab::IoParallel | LauncherTab::IoNetworking | LauncherTab::IoAudio => {
                LauncherTab::IoPorts
            }
            other => other,
        }
    }

    /// The parent tab a sub-page returns to via its Back button, or `None` when
    /// the page has no Back (the A/V categories switch between each other via the
    /// top nav row instead).
    pub fn parent_tab(self) -> Option<LauncherTab> {
        match self {
            // With the library, WHDLoad is a strip entry of its own and its
            // two pages switch between each other on the nav row, so there
            // is nowhere for Back to go. Without it, the one page is still
            // a Storage sub-page.
            #[cfg(not(feature = "game-library"))]
            LauncherTab::Whdload => Some(LauncherTab::Storage),
            LauncherTab::Cd
            | LauncherTab::HostFs
            | LauncherTab::HostDisk
            | LauncherTab::BootPriority
            | LauncherTab::Lide
            | LauncherTab::CreateFloppy
            | LauncherTab::CreateHard => Some(LauncherTab::Storage),
            // Back goes to the page that sent you here, not to Storage.
            LauncherTab::CreateGeometry => Some(LauncherTab::CreateHard),
            LauncherTab::FluxBridge => Some(LauncherTab::Floppy),
            _ => None,
        }
    }

    /// The top nav row's buttons -- sibling pages reachable from here as
    /// `(label, tab)` pairs. Empty when the page shows a Back button instead.
    /// The button whose tab is the current page is drawn highlighted.
    pub fn nav_options(self) -> &'static [(&'static str, LauncherTab)] {
        match self {
            LauncherTab::Storage => STORAGE_NAV,
            LauncherTab::AvAudio
            | LauncherTab::AvVideo
            | LauncherTab::AvEmulation
            | LauncherTab::AvPaths => AV_NAV,
            LauncherTab::IoPorts
            | LauncherTab::IoParallel
            | LauncherTab::IoNetworking
            | LauncherTab::IoAudio => IO_NAV,
            LauncherTab::CreateFloppy | LauncherTab::CreateHard => CREATE_NAV,
            #[cfg(feature = "game-library")]
            LauncherTab::Whdload | LauncherTab::WhdloadLibrary => WHDLOAD_NAV,
            _ => &[],
        }
    }

    /// Whether this tab shows the nav row (its sibling-page links or a Back
    /// button) at the top of the pane, above its settings.
    pub fn has_top_nav(self) -> bool {
        !self.nav_options().is_empty() || self.parent_tab().is_some()
    }
}

/// The Storage tab's top nav links (its sub-pages), left to right.
const STORAGE_NAV: &[(&str, LauncherTab)] = &[
    ("Host Folder", LauncherTab::HostFs),
    ("Host Disk", LauncherTab::HostDisk),
    ("Boot Priority", LauncherTab::BootPriority),
    // Last of the four, because it is the one entry that makes something
    // rather than attaching something: nothing on its pages describes this
    // machine, which is why they are pages of their own.
    ("Create Image...", LauncherTab::CreateFloppy),
    // Four to a row, so these two wrap onto a second.
    ("CD", LauncherTab::Cd),
    ("Lide", LauncherTab::Lide),
];

/// The workshop's two pages. Reached from Storage, so they show a Back
/// button *and* this nav: one says where you came from, the other which of
/// the two you are on.
/// WHDLoad's two pages: what a package boots with, and which package.
/// Only a build with the library has the second, so only that one splits.
#[cfg(feature = "game-library")]
const WHDLOAD_NAV: &[(&str, LauncherTab)] = &[
    // The library first: it is what the strip entry opens on, and what
    // somebody is there for. The settings behind it are one click away.
    ("Library", LauncherTab::WhdloadLibrary),
    ("Settings...", LauncherTab::Whdload),
];

const CREATE_NAV: &[(&str, LauncherTab)] = &[
    ("Floppy Disk", LauncherTab::CreateFloppy),
    ("Hard Disk", LauncherTab::CreateHard),
];

/// The I/O Ports categories, left to right. `IoPorts` is the default,
/// so its button reads "Serial Port".
const IO_NAV: &[(&str, LauncherTab)] = &[
    ("Serial Port", LauncherTab::IoPorts),
    ("Parallel Port", LauncherTab::IoParallel),
    ("Networking", LauncherTab::IoNetworking),
    ("Audio", LauncherTab::IoAudio),
];

/// The A/V & Emu categories, left to right (matching "A/V"). `AvAudio` is the
/// default, so its button reads "Audio".
const AV_NAV: &[(&str, LauncherTab)] = &[
    ("Audio", LauncherTab::AvAudio),
    ("Video", LauncherTab::AvVideo),
    ("Emulation", LauncherTab::AvEmulation),
    ("Paths", LauncherTab::AvPaths),
];

/// A single editable setting. Parameter-free variants keep the per-tab row
/// tables and `UiControl` hit-testing simple (every control is one `Copy` enum
/// value); the floppy/SCSI families are spelled out rather than indexed for the
/// same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherField {
    // --- the Paths page ---------------------------------------------
    //
    // The configuration's `[paths]` section, edited here and written out
    // with the rest of it. On `MachineSetup` because that is the
    // launcher's edit buffer for everything a configuration holds.
    PathsBase,
    PathsStates,
    PathsScreenshots,
    PathsRecordings,
    PathsNvram,
    PathsTraces,
    PathsConfigs,
    PathsRoms,
    PathsFloppies,
    PathsHarddrives,
    PathsCds,
    // Create Image workshop -- these edit no machine setting, only what the
    // next image will be made of.
    NewFloppyDensity,
    NewFloppyContainer,
    NewFloppyFs,
    NewFloppyFsVariant,
    NewFloppyLabel,
    NewFloppyBootable,
    NewFloppyCreate,
    NewHardSize,
    NewHardGeometryMode,
    NewHardPartitioning,
    NewHardDevice,
    NewHardFs,
    NewHardFsVariant,
    NewHardLabel,
    NewHardBootable,
    NewHardBootPri,
    NewHardReadOnly,
    NewHardSparse,
    NewHardCreate,
    NewGeomCylinders,
    NewGeomSurfaces,
    NewGeomSectors,
    NewGeomReserved,
    NewGeomVendor,
    NewGeomProduct,
    NewGeomRevision,
    NewGeomSave,
    NewGeomAuto,
    // System
    Chipset,
    Agnus,
    Denise,
    Video,
    Rtc,
    Identify,
    Rtg,
    // CPU
    Cpu,
    Fpu,
    Clock,
    Icache,
    Dcache,
    Jit,
    // Memory
    ChipRam,
    FastRam,
    SlowRam,
    RamInit,
    RamPattern,
    MbRam,
    AccelRam,
    Z3Ram,
    // ROM
    Rom,
    ExtendedRom,
    // Floppy
    FloppyDrives,
    FloppySpeed,
    Df0Image,
    Df0WriteProtect,
    Df1Image,
    Df1WriteProtect,
    Df2Image,
    Df2WriteProtect,
    Df3Image,
    Df3WriteProtect,
    // The per-drive "use a real drive" tick boxes, and the settings behind the
    // Configure button they reveal. The settings are one set of rows shown for
    // whichever bay is being configured, rather than four copies.
    Df0Bridge,
    Df1Bridge,
    Df2Bridge,
    Df3Bridge,
    /// The greyed heading naming the installed library and its version. Inert:
    /// it labels the page rather than editing anything.
    BridgeLibrary,
    /// A line of the explanation shown in place of the settings when there is
    /// no library to apply them to. Inert, and shared by every such line.
    BridgeLibraryHelp,
    BridgeDevice,
    BridgePort,
    BridgeCable,
    BridgeDensity,
    BridgeReadMode,
    BridgeReplaySpeed,
    // Hard disk
    IdeMaster,
    IdeSlave,
    ScsiController,
    ScsiRom,
    ScsiRomOdd,
    ScsiUnit0,
    ScsiUnit1,
    ScsiUnit2,
    ScsiUnit3,
    ScsiUnit4,
    ScsiUnit5,
    ScsiUnit6,
    // The `[lide]` built-in Zorro II IDE board, on its own Storage sub-page
    // rather than crowding the Storage tab's own 12 rows.
    LideBoard,
    LideRom,
    LideRomBank2,
    LideDrive0,
    LideDrive1,
    LideDrive2,
    LideDrive3,
    // Boot priority sub-page: the synthesized-RDB de_BootPri for each hard-disk
    // drive above, edited on its own page so it does not crowd the Storage tab.
    IdeMasterBoot,
    IdeSlaveBoot,
    ScsiUnit0Boot,
    ScsiUnit1Boot,
    ScsiUnit2Boot,
    ScsiUnit3Boot,
    ScsiUnit4Boot,
    ScsiUnit5Boot,
    ScsiUnit6Boot,
    LideDrive0Boot,
    LideDrive1Boot,
    LideDrive2Boot,
    LideDrive3Boot,
    // Host FS mounts (the GUI edits the first FILESYS_GUI_SLOTS entries)
    Filesys0Dir,
    Filesys0Boot,
    Filesys0ReadOnly,
    Filesys1Dir,
    Filesys1Boot,
    Filesys1ReadOnly,
    Filesys2Dir,
    Filesys2Boot,
    Filesys2ReadOnly,
    Filesys3Dir,
    Filesys3Boot,
    Filesys3ReadOnly,
    // CD
    CdImage,
    CdInsertDelay,
    Cd32Nvram,
    // WHDLoad direct boot (the Storage tab's WHDLoad sub-page)
    WhdloadGame,
    WhdloadKickstarts,
    WhdloadLibrary,
    WhdloadWhdPackage,
    WhdloadSkickPackage,
    WhdloadMachine,
    WhdloadOpenRetro,
    WhdloadEnabled,
    WhdloadGames,
    // Serial. Present only with the `midi` feature, the only build carrying
    // serial rows at all.
    #[cfg(feature = "midi")]
    SerialMode,
    /// The remote `host:port` the port dials in `tcp-connect` mode, typed
    /// into the Serial section's Connect box.
    #[cfg(feature = "midi")]
    SerialConnect,
    /// The local address the port binds in `tcp` mode, typed into the
    /// Serial section's Listen box.
    #[cfg(feature = "midi")]
    SerialListen,
    #[cfg(feature = "midi")]
    MidiOut,
    Mt32ControlRom,
    Mt32PcmRom,
    Mt32Panel,
    Mt32Lcd,
    #[cfg(feature = "midi")]
    MidiIn,
    /// Coppersynth's soundfont (.sf2); unset means the bundled
    /// default's search path.
    #[cfg(feature = "coppersynth")]
    CsynthSoundfont,
    CsynthPanel,
    /// The MT-32 mode of Coppersynth: Auto / On / Off.
    #[cfg(feature = "coppersynth")]
    CsynthMt32Mode,
    // Parallel
    ParallelDevice,
    ParallelOutput,
    SamplerInput,
    SamplerGain,
    /// The A2065 Ethernet board: absent, or fitted with a chosen host
    /// backend (isolated / loopback / NAT).
    Ethernet,
    /// Host adapter used while the A2065 backend is bridged.
    EthernetInterface,
    /// The bundled HostSocket bsdsocket.library board: absent, or fitted
    /// with a chosen host backend.
    HostSocket,
    /// Host adapter used while the HostSocket backend is bridged.
    HostSocketInterface,
    /// The MacroSystem Toccata sound board: fitted or not (`[toccata]
    /// enabled`). No other options exist (see docs/internals/toccata.md).
    Toccata,
    /// The MHI virtual MPEG audio decoder board: fitted or not (`[mhi]
    /// enabled`). No other options exist (see docs/internals/mhi.md). Present
    /// only in an `mhi` build, the only build that can fit the board.
    #[cfg(feature = "mhi")]
    Mhi,
    /// Inert field for a non-interactive [`RowKind::SectionHeader`] row.
    SectionHeader,
    // A/V and emulation
    AudioDevice,
    AudioChannelMode,
    AudioStereoSeparation,
    AudioFilter,
    Overscan,
    PixelAspect,
    Scaling,
    Tint,
    Deinterlace,
    Phosphor,
    Shader,
    ShaderStrength,
    Bezel,
    PerfOverlay,
    MenuScale,
    StartFullscreen,
    ShowStatusBar,
    FloppySounds,
    FloppyVolume,
    PowerOn,
    PacingBudget,
    RealtimePriority,
    Warp,
    // Input
    Joystick,
    MouseSensitivity,
    MouseCapture,
    Port1Device,
    Port2Device,
}

/// How a row's value is edited, and therefore which widget the panel draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A `[<] value [>]` picker / stepper.
    Cycle,
    /// A `[<] value [>]` stepper whose value is also a text field: the arrows
    /// nudge it by one and clicking the value types an exact number. Used for
    /// the hard-disk boot priorities, where any value in -128..=127 is valid.
    Bootpri,
    /// An On/Off button.
    Toggle,
    /// A file path with Browse/Clear buttons.
    Path,
    /// A hard-drive image: a path with Browse/Clear, plus an editable
    /// volume-name field (used when the image is a host directory).
    Drive,
    /// The greyed column headers of a ROM row's identification table.
    RomInfoHeader,
    /// The identification itself: Name / Version / Revision cells, blank
    /// when the slot is empty or the image is unrecognised.
    RomInfoValue,
    /// A non-interactive greyed heading that groups the rows beneath it
    /// (e.g. the `Serial:` / `Parallel:` sections of the I/O Ports tab). Its
    /// `field` is inert.
    SectionHeader,
    /// The greyed `Drive` / `Priority` / `Status` column titles above the Boot
    /// Priority rows. Non-interactive; its `field` is inert.
    BootpriHeader,
    /// A greyed, non-interactive line under the row it belongs to, carrying
    /// something looked up rather than set: the ROM tab's identification of
    /// the chosen image (see [`crate::romdb`]). Its `field` is the row it
    /// annotates, so it is hidden and greyed along with it, and it draws
    /// nothing when there is nothing to say.
    Note,
    /// A floppy drive's media row: an image path with Browse/Clear, or the
    /// real interface in use with a Configure button once bridged.
    FloppyMedia,
    /// The pair of tick boxes under a drive: write protect, and whether the
    /// bay uses a real drive. Its `field` is the drive's write-protect field.
    FloppyFlags,
    /// A free-text value box: click it to type. Unlike [`RowKind::Path`] it
    /// holds a word rather than a file, so it has no Browse button.
    Text,
    /// A button that does something, drawn where a value would be. The row
    /// label is blank and the button carries the wording.
    Action,
    /// A typed number with the unit it is in written beside it; clicking
    /// the unit swaps it. Used for the hard-drive size, where the useful
    /// range is far too wide for a stepper.
    Size,
    /// The geometry mode: Auto and Custom side by side, with a Configure
    /// button appearing beside them once Custom is chosen.
    GeometryMode,
    /// A typed whole number in a plain box, lined up with the value column.
    /// Used where the useful range is too wide to walk with arrows.
    Number,
    /// A typed whole number with a stepper either side, for the geometry
    /// figures: the arrows nudge by one, the box takes an exact value.
    Stepper,
    /// The filesystem family, as a row of tick boxes: which handler the
    /// volume is for, one of them always chosen.
    FsFamily,
    /// The filesystem variant, on the row directly under the family: the
    /// options AmigaDOS's own filesystem carries, greyed for a family that
    /// has none.
    FsVariant,
    /// An account: whether this session is signed in, with the button that
    /// signs it in where a Browse would be. Nothing is stored, so the row
    /// reports the session rather than a setting.
    Account,
}

/// One settings row: a label, the field it edits, and how to edit it.
#[derive(Debug, Clone, Copy)]
pub struct Row {
    pub field: LauncherField,
    pub label: &'static str,
    pub kind: RowKind,
}

const fn row(field: LauncherField, label: &'static str, kind: RowKind) -> Row {
    Row { field, label, kind }
}

/// A non-interactive section heading row (see [`RowKind::SectionHeader`]).
const fn section_header(label: &'static str) -> Row {
    Row {
        field: F::SectionHeader,
        label,
        kind: RowKind::SectionHeader,
    }
}

/// The greyed column-title row on the Boot Priority page (see
/// [`RowKind::BootpriHeader`]).
const fn bootpri_header() -> Row {
    Row {
        field: F::SectionHeader,
        label: "",
        kind: RowKind::BootpriHeader,
    }
}

/// How many `[[filesys]]` mounts the launcher edits (the config file
/// accepts more; extras round-trip untouched).
pub const FILESYS_GUI_SLOTS: usize = 4;

/// The Host FS mount slot a launcher field addresses, or `None` for other
/// fields: (mount index, whether the field is the boot-priority row).
fn filesys_slot(field: LauncherField) -> Option<(usize, bool)> {
    Some(match field {
        LauncherField::Filesys0Dir => (0, false),
        LauncherField::Filesys0Boot => (0, true),
        LauncherField::Filesys1Dir => (1, false),
        LauncherField::Filesys1Boot => (1, true),
        LauncherField::Filesys2Dir => (2, false),
        LauncherField::Filesys2Boot => (2, true),
        LauncherField::Filesys3Dir => (3, false),
        LauncherField::Filesys3Boot => (3, true),
        _ => return None,
    })
}

/// The Host FS mount slot of an Access (read-only) spinner field.
fn filesys_readonly_slot(field: LauncherField) -> Option<usize> {
    Some(match field {
        LauncherField::Filesys0ReadOnly => 0,
        LauncherField::Filesys1ReadOnly => 1,
        LauncherField::Filesys2ReadOnly => 2,
        LauncherField::Filesys3ReadOnly => 3,
        _ => return None,
    })
}

impl LauncherField {
    /// Whether this field is a Host FS mount's directory (folder picker),
    /// as opposed to a boot-priority stepper or any other field.
    pub fn is_filesys_dir_field(self) -> bool {
        matches!(filesys_slot(self), Some((_, false)))
    }

    /// Whether this field is a row on the Paths page: a directory in
    /// `[paths]` rather than anything belonging to the machine. They get
    /// a folder picker, they show the whole path rather than a file name,
    /// and they take effect the moment they change.
    pub fn is_paths_field(self) -> bool {
        matches!(
            self,
            LauncherField::PathsBase
                | LauncherField::PathsStates
                | LauncherField::PathsScreenshots
                | LauncherField::PathsRecordings
                | LauncherField::PathsNvram
                | LauncherField::PathsTraces
                | LauncherField::PathsConfigs
                | LauncherField::PathsRoms
                | LauncherField::PathsFloppies
                | LauncherField::PathsHarddrives
                | LauncherField::PathsCds
        )
    }

    /// Whether this field is a WHDLoad staging directory (folder picker):
    /// the Kickstart-image and game-library directories, but not the game
    /// package (an `.lha` file, picked as a file).
    pub fn is_whdload_dir_field(self) -> bool {
        matches!(
            self,
            LauncherField::WhdloadKickstarts | LauncherField::WhdloadLibrary
        ) || cfg!(feature = "game-library") && self.is_whdload_games_field()
    }

    /// The game library, which is a folder of packages rather than one of
    /// them. Its own predicate because the field only exists in a build
    /// with the library, and `matches!` cannot be written conditionally.
    pub fn is_whdload_games_field(self) -> bool {
        #[cfg(feature = "game-library")]
        {
            self == LauncherField::WhdloadGames
        }
        #[cfg(not(feature = "game-library"))]
        {
            false
        }
    }

    /// Whether this field names a `.lha` archive rather than a directory:
    /// the game to launch, and the two support packages.
    pub fn is_whdload_archive_field(self) -> bool {
        matches!(
            self,
            LauncherField::WhdloadGame
                | LauncherField::WhdloadWhdPackage
                | LauncherField::WhdloadSkickPackage
        )
    }

    /// Whether this field is one of the WHDLoad host paths (game package or
    /// staging directory), which show the whole host path like the Host FS
    /// mounts do.
    pub fn is_whdload_path_field(self) -> bool {
        matches!(
            self,
            LauncherField::WhdloadGame
                | LauncherField::WhdloadGames
                | LauncherField::WhdloadKickstarts
                | LauncherField::WhdloadLibrary
                | LauncherField::WhdloadWhdPackage
                | LauncherField::WhdloadSkickPackage
        )
    }
}

/// What a hard-disk slot holds. The two are interchangeable to everything
/// that only wants to know whether the slot is occupied.
enum DriveContents<'a> {
    Image(&'a Path),
    HostDisk,
}

use LauncherField as F;
use RowKind::{Bootpri, Cycle, Drive, Toggle};
// `RowKind::Path` is written out below so it does not collide with the
// `std::path::Path` import.
use RowKind::Path as PathRow;

const SYSTEM_ROWS: [Row; 7] = [
    row(F::Chipset, "Chipset", Cycle),
    row(F::Agnus, "Agnus", Cycle),
    row(F::Denise, "Denise", Cycle),
    row(F::Video, "Video", Cycle),
    row(F::Rtc, "Real-time clock", Cycle),
    row(F::Identify, "Identify board", Cycle),
    row(F::Rtg, "RTG card", Cycle),
];
const CPU_ROWS: [Row; 6] = [
    row(F::Cpu, "CPU", Cycle),
    row(F::Fpu, "FPU (68881/2)", Cycle),
    row(F::Clock, "Clock", Cycle),
    row(F::Icache, "Instruction cache", Cycle),
    row(F::Dcache, "Data cache", Cycle),
    row(F::Jit, "JIT accelerator", Cycle),
];
const MEMORY_ROWS: [Row; 8] = [
    row(F::ChipRam, "Chip RAM", Cycle),
    row(F::FastRam, "Fast RAM", Cycle),
    row(F::SlowRam, "Slow RAM", Cycle),
    // Below the sizes most people came for: what the bits hold at
    // power-on, and (only while the fill is Fixed) the word they hold.
    row(F::RamInit, "Power-on fill", Cycle),
    row(F::RamPattern, "Fill pattern", RowKind::Text),
    row(F::MbRam, "Motherboard RAM", Cycle),
    row(F::AccelRam, "Accelerator RAM", Cycle),
    row(F::Z3Ram, "Zorro III RAM", Cycle),
];
// Each ROM row is followed by a greyed table naming what the chosen
// image checksums to, split into Name / Version / Revision columns
// ("Kickstart", "3.1", "40.68"), since a ROM file's name says only what
// its dumper called it. The table stands whether or not an image is
// loaded; empty cells mean an empty (or unrecognised) slot.
const ROM_ROWS: [Row; 7] = [
    section_header("Primary ROM:"),
    row(F::Rom, "  Kickstart ROM", PathRow),
    row(F::Rom, "", RowKind::RomInfoHeader),
    row(F::Rom, "", RowKind::RomInfoValue),
    // A blank header as a spacer: the next section starts a row clear
    // of the table. The extended slot gets no table of its own -- the
    // checksum table rarely names one, and a blank grid says nothing.
    section_header(""),
    section_header("Extended ROM:"),
    row(F::ExtendedRom, "  Extended ROM", PathRow),
];
// Each drive is a greyed "DFn:" heading with its settings indented under it. The
// heading is keyed on the drive's image field so `row_hidden` drops it along
// with the drive's rows when the drive is not wired in.
// Each wired drive is a greyed "DFn:" heading, its media row, then the two
// tick boxes that share a line beneath. The media row shows an image path with
// Browse/Clear, or -- once FluxBridge is ticked -- the real interface in use
// with a Configure button onto its settings.
const FLOPPY_ROWS: [Row; 14] = [
    row(F::FloppyDrives, "Drives", Cycle),
    row(F::FloppySpeed, "Drive speed", Cycle),
    row(F::Df0Image, "DF0:", RowKind::SectionHeader),
    row(F::Df0Image, "  Disk image", RowKind::FloppyMedia),
    row(F::Df0WriteProtect, "", RowKind::FloppyFlags),
    row(F::Df1Image, "DF1:", RowKind::SectionHeader),
    row(F::Df1Image, "  Disk image", RowKind::FloppyMedia),
    row(F::Df1WriteProtect, "", RowKind::FloppyFlags),
    row(F::Df2Image, "DF2:", RowKind::SectionHeader),
    row(F::Df2Image, "  Disk image", RowKind::FloppyMedia),
    row(F::Df2WriteProtect, "", RowKind::FloppyFlags),
    row(F::Df3Image, "DF3:", RowKind::SectionHeader),
    row(F::Df3Image, "  Disk image", RowKind::FloppyMedia),
    row(F::Df3WriteProtect, "", RowKind::FloppyFlags),
];
/// The FluxBridge settings page, shown for whichever bay was configured.
#[cfg(feature = "fluxbridge")]
const FLOPPY_BRIDGE_ROWS: [Row; 7] = [
    // Inert: the label is built from the loaded library's version (see
    // `bridge_library_heading`), so the text here is never drawn.
    row(F::BridgeLibrary, "", RowKind::SectionHeader),
    row(F::BridgeDevice, "Interface", Cycle),
    row(F::BridgePort, "Serial port", Cycle),
    row(F::BridgeCable, "Drive select", Cycle),
    row(F::BridgeDensity, "Density", Cycle),
    row(F::BridgeReadMode, "Read mode", Cycle),
    row(F::BridgeReplaySpeed, "Replay speed", Cycle),
];
const STORAGE_ROWS: [Row; 12] = [
    row(F::IdeMaster, "IDE master", Drive),
    row(F::IdeSlave, "IDE slave", Drive),
    row(F::ScsiController, "SCSI controller", Cycle),
    row(F::ScsiRom, "SCSI boot ROM", PathRow),
    row(F::ScsiRomOdd, "SCSI ROM (odd)", PathRow),
    row(F::ScsiUnit0, "SCSI unit 0", Drive),
    row(F::ScsiUnit1, "SCSI unit 1", Drive),
    row(F::ScsiUnit2, "SCSI unit 2", Drive),
    row(F::ScsiUnit3, "SCSI unit 3", Drive),
    row(F::ScsiUnit4, "SCSI unit 4", Drive),
    row(F::ScsiUnit5, "SCSI unit 5", Drive),
    row(F::ScsiUnit6, "SCSI unit 6", Drive),
];
const HOSTFS_ROWS: [Row; 12] = [
    row(F::Filesys0Dir, "HOSTFS0", Drive),
    row(F::Filesys0Boot, "  Boot priority", Cycle),
    row(F::Filesys0ReadOnly, "  Access", Cycle),
    row(F::Filesys1Dir, "HOSTFS1", Drive),
    row(F::Filesys1Boot, "  Boot priority", Cycle),
    row(F::Filesys1ReadOnly, "  Access", Cycle),
    row(F::Filesys2Dir, "HOSTFS2", Drive),
    row(F::Filesys2Boot, "  Boot priority", Cycle),
    row(F::Filesys2ReadOnly, "  Access", Cycle),
    row(F::Filesys3Dir, "HOSTFS3", Drive),
    row(F::Filesys3Boot, "  Boot priority", Cycle),
    row(F::Filesys3ReadOnly, "  Access", Cycle),
];
// One boot-priority row per hard-disk drive, matching the Storage tab's drive
// rows. Greyed when the matching slot holds no image.
// IDE and SCSI only: lide's drives get their own priority rows on the Lide
// page (see `LIDE_ROWS`) rather than crowding this table -- with them added
// here the page no longer fits (`every_launcher_tab_row_fits_inside_the_panel`).
const BOOTPRI_ROWS: [Row; 9] = [
    row(F::IdeMasterBoot, "IDE master", Bootpri),
    row(F::IdeSlaveBoot, "IDE slave", Bootpri),
    row(F::ScsiUnit0Boot, "SCSI unit 0", Bootpri),
    row(F::ScsiUnit1Boot, "SCSI unit 1", Bootpri),
    row(F::ScsiUnit2Boot, "SCSI unit 2", Bootpri),
    row(F::ScsiUnit3Boot, "SCSI unit 3", Bootpri),
    row(F::ScsiUnit4Boot, "SCSI unit 4", Bootpri),
    row(F::ScsiUnit5Boot, "SCSI unit 5", Bootpri),
    row(F::ScsiUnit6Boot, "SCSI unit 6", Bootpri),
];
const CD_ROWS: [Row; 3] = [
    row(F::CdImage, "CD image", PathRow),
    row(F::CdInsertDelay, "Insert delay", Cycle),
    row(F::Cd32Nvram, "CD32 NVRAM", PathRow),
];
// The `[lide]` Storage sub-page: board personality, boot ROM(s), up to four
// drives (RIPPLE's two channels; RIDE/AT-Bus 2008 hide slots 2-3, and AT-Bus
// 2008 also hides the second ROM bank -- it has no flash banking), and each
// drive's boot priority. Boot priority sits here rather than on the shared
// Boot Priority page: with lide's four slots added there it stopped fitting.
const LIDE_ROWS: [Row; 11] = [
    row(F::LideBoard, "Board", Cycle),
    row(F::LideRom, "Boot ROM", PathRow),
    row(F::LideRomBank2, "Boot ROM bank 2", PathRow),
    row(F::LideDrive0, "Drive 0", Drive),
    row(F::LideDrive1, "Drive 1", Drive),
    row(F::LideDrive2, "Drive 2", Drive),
    row(F::LideDrive3, "Drive 3", Drive),
    row(F::LideDrive0Boot, "  Boot priority", Bootpri),
    row(F::LideDrive1Boot, "  Boot priority", Bootpri),
    row(F::LideDrive2Boot, "  Boot priority", Bootpri),
    row(F::LideDrive3Boot, "  Boot priority", Bootpri),
];
// The WHDLoad Settings page: the game to launch, then what staging
// draws on (src/whdload.rs). Drive rows like the Host FS mounts so the
// whole host path shows; the staged volumes mount under fixed names
// (WHDBoot:/WHDGame:), so there is no volume box to fill.
//
// Pinning, the account and the game folder belong to the game library, so
// they are only here in a build that has one. Their settings still
// round-trip through a save either way -- a configuration written by a full
// build loads in a slim one without losing them.
#[cfg(not(feature = "game-library"))]
const WHDLOAD_ROWS: [Row; 8] = [
    section_header("WHDLoad Settings:"),
    // What to boot, and how: what a person changes per game.
    row(F::WhdloadGame, "Launch game", Drive),
    row(F::WhdloadMachine, "Machine type", Cycle),
    // Then the places things live, set once and left.
    section_header("Directories:"),
    row(F::WhdloadWhdPackage, "WHDLoad package", Drive),
    row(F::WhdloadSkickPackage, "SKick package", Drive),
    row(F::WhdloadKickstarts, "Kickstart ROMs", Drive),
    row(F::WhdloadLibrary, "Save data", Drive),
];
#[cfg(feature = "game-library")]
const WHDLOAD_ROWS: [Row; 10] = [
    section_header("WHDLoad Settings:"),
    // What to boot, and how: what a person changes per game.
    row(F::WhdloadGame, "Launch game", Drive),
    row(F::WhdloadMachine, "Machine type", Cycle),
    row(F::WhdloadOpenRetro, "OpenRetro", RowKind::Account),
    // Then the places things live, set once and left.
    section_header("Directories:"),
    row(F::WhdloadWhdPackage, "WHDLoad package", Drive),
    row(F::WhdloadSkickPackage, "SKick package", Drive),
    row(F::WhdloadKickstarts, "Kickstart ROMs", Drive),
    row(F::WhdloadGames, "Game library", Drive),
    row(F::WhdloadLibrary, "Save data", Drive),
];
// The MIDI endpoint rows appear only when the serial port is in MIDI mode, so
// the Serial section shows just the Device / Mode selector otherwise. The
// selector is labelled "Device / Mode" because some choices are devices (MIDI)
// and some are modes (stdout, PTY, TCP).
// Rows under each I/O Ports section heading are indented two spaces so they
// read as belonging to their `Serial:` / `Parallel:` / `Ethernet:` port.
#[cfg(feature = "midi")]
const SERIAL_ROWS_BASE: [Row; 1] = [row(F::SerialMode, "  Device / Mode", Cycle)];
// The two TCP modes each carry one address, and only one: dialling out needs
// somewhere to dial, listening needs somewhere to bind. Each box shows only
// under the mode it belongs to, so neither mode offers the other's address.
#[cfg(feature = "midi")]
const SERIAL_ROWS_TCP_CONNECT: [Row; 2] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::SerialConnect, "  Connect", RowKind::Text),
];
#[cfg(feature = "midi")]
const SERIAL_ROWS_TCP_LISTEN: [Row; 2] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::SerialListen, "  Listen", RowKind::Text),
];
#[cfg(feature = "midi")]
const SERIAL_ROWS_MIDI: [Row; 3] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::MidiIn, "  MIDI input", Cycle),
    row(F::MidiOut, "  MIDI output", Cycle),
];
// Picking MT-32 as the output adds the two ROM images it runs on and
// its front panel; nothing else needs them, so nothing else shows them.
#[cfg(all(feature = "midi", feature = "mt32"))]
const SERIAL_ROWS_MT32: [Row; 7] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::MidiIn, "  MIDI input", Cycle),
    row(F::MidiOut, "  MIDI output", Cycle),
    row(F::Mt32ControlRom, "  Control ROM", PathRow),
    row(F::Mt32PcmRom, "  PCM ROM", PathRow),
    row(F::Mt32Panel, "  Front panel", Cycle),
    row(F::Mt32Lcd, "  Display", Cycle),
];
// Coppersynth needs no ROMs: its rows are the soundfont it
// plays and whether the MT-32 translation layer sits in front of it.
#[cfg(all(feature = "midi", feature = "coppersynth"))]
const SERIAL_ROWS_CSYNTH: [Row; 6] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::MidiIn, "  MIDI input", Cycle),
    row(F::MidiOut, "  MIDI output", Cycle),
    row(F::CsynthSoundfont, "  SoundFont", PathRow),
    row(F::CsynthPanel, "  Front panel", Cycle),
    row(F::CsynthMt32Mode, "  MT-32 mode", Cycle),
];
// The sampler input/gain rows appear only when the sampler is the selected
// device, so None/Printer show just the Device selector.
const PARALLEL_ROWS_BASE: [Row; 1] = [row(F::ParallelDevice, "  Device", Cycle)];
// The printer adds a capture-file picker; the sampler adds its input/gain rows.
const PARALLEL_ROWS_PRINTER: [Row; 2] = [
    row(F::ParallelDevice, "  Device", Cycle),
    row(F::ParallelOutput, "  Output file", PathRow),
];
const PARALLEL_ROWS_SAMPLER: [Row; 3] = [
    row(F::ParallelDevice, "  Device", Cycle),
    row(F::SamplerInput, "  Audio input", Cycle),
    row(F::SamplerGain, "  Input gain", Cycle),
];
const ETHERNET_ROWS: [Row; 4] = [
    row(F::Ethernet, "  A2065", Cycle),
    row(F::EthernetInterface, "  Host adapter", Cycle),
    row(F::HostSocket, "  HostSocket", Cycle),
    row(F::HostSocketInterface, "  Host adapter", Cycle),
];
// Both boards are a single fit/don't-fit toggle -- no host backend, no other
// options (see docs/internals/toccata.md and docs/internals/mhi.md). Host
// audio capture/backend settings (wav capture, stems, device selection)
// intentionally stay command-line/config-file only and have no row here.
#[cfg(feature = "mhi")]
const SOUND_ROWS: [Row; 2] = [
    row(F::Toccata, "  Toccata", Cycle),
    row(F::Mhi, "  MHI decoder", Cycle),
];
#[cfg(not(feature = "mhi"))]
const SOUND_ROWS: [Row; 1] = [row(F::Toccata, "  Toccata", Cycle)];
// The A/V & Emu tab is split into three categories switched via the top nav row.
// The Video category also carries the CRT-shader controls (a picture setting).
const VIDEO_ROWS: [Row; 13] = [
    row(F::StartFullscreen, "Start fullscreen", Cycle),
    row(F::ShowStatusBar, "Status bar", Cycle),
    row(F::Bezel, "Monitor bezel", Cycle),
    row(F::PerfOverlay, "Perf overlay", Cycle),
    row(F::MenuScale, "Menu size", Cycle),
    row(F::Overscan, "Overscan", Cycle),
    row(F::PixelAspect, "Pixel aspect", Cycle),
    row(F::Scaling, "Scaling", Cycle),
    row(F::Deinterlace, "Deinterlace", Cycle),
    row(F::Tint, "Screen tint", Cycle),
    row(F::Phosphor, "Phosphor", Cycle),
    row(F::Shader, "CRT shader", Cycle),
    row(F::ShaderStrength, "Shader strength", Cycle),
];
const AUDIO_ROWS: [Row; 6] = [
    row(F::AudioDevice, "Audio output", Cycle),
    row(F::AudioChannelMode, "Channel mode", Cycle),
    row(F::AudioStereoSeparation, "Stereo separation", Cycle),
    row(F::AudioFilter, "Audio filter", Cycle),
    row(F::FloppySounds, "Floppy sounds", Cycle),
    row(F::FloppyVolume, "Floppy volume", Cycle),
];
#[cfg(not(feature = "game-library"))]
const EMULATION_ROWS: [Row; 4] = [
    row(F::PowerOn, "Power on startup", Cycle),
    row(F::RealtimePriority, "Realtime priority", Cycle),
    row(F::PacingBudget, "Pacing budget", Cycle),
    row(F::Warp, "Warp speed", Cycle),
];
#[cfg(feature = "game-library")]
const EMULATION_ROWS: [Row; 5] = [
    row(F::PowerOn, "Power on startup", Cycle),
    row(F::RealtimePriority, "Realtime priority", Cycle),
    row(F::PacingBudget, "Pacing budget", Cycle),
    row(F::Warp, "Warp speed", Cycle),
    // Off, the strip loses its WHDLoad entry and the pages behind it stop
    // doing anything at all -- no database read, no cover worker, no scan.
    row(F::WhdloadEnabled, "WHDLoad", Cycle),
];
/// The Paths page. Every row is optional: cleared, it inherits, and the
/// value shown is the directory that would be used. Nothing here describes
/// the machine, so none of it round-trips through [`RawConfig`].
const PATHS_ROWS: [Row; 12] = [
    row(F::PathsBase, "Base folder", PathRow),
    section_header("Custom directories:"),
    // Indented under the heading, the same as the sections on the I/O
    // Ports and MT-32 pages: the base above it is not one of these, and
    // the indent is what says so.
    row(F::PathsStates, "  Save states", PathRow),
    row(F::PathsScreenshots, "  Screenshots", PathRow),
    row(F::PathsRecordings, "  Recordings", PathRow),
    row(F::PathsNvram, "  NVRAM", PathRow),
    row(F::PathsTraces, "  Traces", PathRow),
    row(F::PathsConfigs, "  Config files", PathRow),
    row(F::PathsRoms, "  ROMs", PathRow),
    row(F::PathsFloppies, "  Floppies", PathRow),
    row(F::PathsHarddrives, "  Hard drives", PathRow),
    row(F::PathsCds, "  CD images", PathRow),
];

/// The floppy page. Every option the format carries is on it; nothing on
/// it reads or writes the machine's configuration.
const NEW_FLOPPY_ROWS: [Row; 8] = [
    section_header("Create Floppy Disk image (ADF):"),
    row(F::NewFloppyDensity, "Density", Cycle),
    row(F::NewFloppyContainer, "Container", Cycle),
    row(F::NewFloppyFs, "Filesystem", RowKind::FsFamily),
    row(F::NewFloppyFsVariant, "DOSType", RowKind::FsVariant),
    row(F::NewFloppyLabel, "Volume name", RowKind::Text),
    row(F::NewFloppyBootable, "Bootable", Toggle),
    row(F::NewFloppyCreate, "", RowKind::Action),
];

const NEW_HARD_ROWS: [Row; 13] = [
    section_header("Create Hard Disk image (HDF):"),
    row(F::NewHardSize, "Size", RowKind::Size),
    row(F::NewHardGeometryMode, "Geometry", RowKind::GeometryMode),
    row(F::NewHardPartitioning, "Partitioning", Cycle),
    row(F::NewHardFs, "Filesystem", RowKind::FsFamily),
    row(F::NewHardFsVariant, "DOSType", RowKind::FsVariant),
    row(F::NewHardDevice, "Device name", RowKind::Text),
    row(F::NewHardLabel, "Volume name", RowKind::Text),
    row(F::NewHardBootable, "Bootable", Toggle),
    row(F::NewHardBootPri, "Boot priority", RowKind::Number),
    row(F::NewHardReadOnly, "Read only", Toggle),
    row(F::NewHardSparse, "Sparse image", Toggle),
    row(F::NewHardCreate, "", RowKind::Action),
];

/// The geometry editor, reached from the hard-disk page.
const NEW_GEOMETRY_ROWS: [Row; 10] = [
    section_header("Custom disk geometry:"),
    row(F::NewGeomCylinders, "Cylinders", RowKind::Stepper),
    // The Amiga's own word for it, and the name of the Rigid Disk Block
    // field this ends up in.
    row(F::NewGeomSurfaces, "Surfaces", RowKind::Stepper),
    row(F::NewGeomSectors, "Sectors per track", RowKind::Stepper),
    row(F::NewGeomReserved, "Reserved blocks", RowKind::Stepper),
    // What the drive answers when asked what it is. HDToolBox shows the
    // first two as its Drive and Type columns.
    section_header("Drive identity:"),
    row(F::NewGeomVendor, "Drive", RowKind::Text),
    row(F::NewGeomProduct, "Type", RowKind::Text),
    row(F::NewGeomRevision, "Revision", RowKind::Text),
    row(F::NewGeomSave, "", RowKind::Action),
];

const INPUT_ROWS: [Row; 5] = [
    row(F::Port1Device, "Port 1", Cycle),
    row(F::Port2Device, "Port 2", Cycle),
    row(F::Joystick, "Joystick input", Cycle),
    row(F::MouseSensitivity, "Mouse sensitivity", Cycle),
    row(F::MouseCapture, "Mouse capture", Cycle),
];

/// The rows shown on a tab, top to bottom. Most tabs are fixed and borrow their
/// static row table; only the composed tabs (the Boot Priority page and the
/// dynamic I/O Ports tab) allocate. The I/O Ports tab is
/// dynamic: the MIDI endpoint rows appear only in MIDI mode and the
/// sampler/printer rows only for those devices, so unrelated options stay hidden
/// rather than greyed. The `Zorro` tab has no rows: it is drawn as a board list
/// with Add/Remove controls (see the panel code).
pub fn rows(
    tab: LauncherTab,
    parallel_device: ParallelDevice,
    serial_mode: SerialMode,
    midi_out_is_mt32: bool,
    midi_out_is_csynth: bool,
) -> Cow<'static, [Row]> {
    match tab {
        LauncherTab::CreateFloppy => Cow::Borrowed(&NEW_FLOPPY_ROWS),
        LauncherTab::CreateHard => Cow::Borrowed(&NEW_HARD_ROWS),
        LauncherTab::CreateGeometry => Cow::Borrowed(&NEW_GEOMETRY_ROWS),
        LauncherTab::System => Cow::Borrowed(&SYSTEM_ROWS),
        LauncherTab::Cpu => Cow::Borrowed(&CPU_ROWS),
        LauncherTab::Memory => Cow::Borrowed(&MEMORY_ROWS),
        LauncherTab::Rom => Cow::Borrowed(&ROM_ROWS),
        LauncherTab::Floppy => Cow::Borrowed(&FLOPPY_ROWS),
        // Unreachable without the feature: nothing offers a way in, since the
        // tick box that turns a bay over is not drawn either.
        #[cfg(not(feature = "fluxbridge"))]
        LauncherTab::FluxBridge => Cow::Borrowed(&[]),
        #[cfg(feature = "fluxbridge")]
        LauncherTab::FluxBridge => Cow::Borrowed(&FLOPPY_BRIDGE_ROWS),
        // The Storage tab shows the IDE/SCSI options (the common case). Its
        // sub-page links are a fixed nav row at the top (see the panel code),
        // in the same place as each sub-page's Back button, so they are not part
        // of the row grid.
        LauncherTab::Storage => Cow::Borrowed(&STORAGE_ROWS),
        LauncherTab::BootPriority => {
            // The greyed column titles, then one row per hard-disk drive.
            let mut rows = vec![bootpri_header()];
            rows.extend_from_slice(&BOOTPRI_ROWS);
            Cow::Owned(rows)
        }
        LauncherTab::HostFs => Cow::Borrowed(&HOSTFS_ROWS),
        LauncherTab::Whdload => Cow::Borrowed(&WHDLOAD_ROWS),
        // The Library draws a list of games rather than rows of settings.
        #[cfg(feature = "game-library")]
        LauncherTab::WhdloadLibrary => Cow::Borrowed(&[]),
        // Drawn as its own layout: a disk table and its buttons, not rows.
        LauncherTab::HostDisk => Cow::Borrowed(&[]),
        LauncherTab::Cd => Cow::Borrowed(&CD_ROWS),
        LauncherTab::Lide => Cow::Borrowed(&LIDE_ROWS),
        LauncherTab::IoPorts => Cow::Owned(io_serial_rows(
            serial_mode,
            midi_out_is_mt32,
            midi_out_is_csynth,
        )),
        LauncherTab::IoParallel => Cow::Owned(io_parallel_rows(parallel_device)),
        LauncherTab::IoNetworking => Cow::Owned(io_networking_rows()),
        LauncherTab::IoAudio => Cow::Owned(io_audio_rows()),
        LauncherTab::Input => Cow::Borrowed(&INPUT_ROWS),
        LauncherTab::Zorro => Cow::Borrowed(&[]),
        // A/V & Emu defaults to the Audio category; Video and Emulation are its
        // sibling categories, switched via the top nav row.
        LauncherTab::AvAudio => Cow::Borrowed(&AUDIO_ROWS),
        LauncherTab::AvVideo => Cow::Borrowed(&VIDEO_ROWS),
        LauncherTab::AvEmulation => Cow::Borrowed(&EMULATION_ROWS),
        LauncherTab::AvPaths => Cow::Borrowed(&PATHS_ROWS),
    }
}

/// The I/O Ports pages, one section each: `Serial:` (only in a `midi`
/// build, which is the only build with serial rows), `Parallel:`,
/// `Ethernet:` and `Audio:`, each under its greyed heading and each
/// showing only the rows relevant to its selected device/mode.
fn io_serial_rows(
    serial_mode: SerialMode,
    midi_out_is_mt32: bool,
    midi_out_is_csynth: bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let serial = serial_rows(serial_mode, midi_out_is_mt32, midi_out_is_csynth);
    if !serial.is_empty() {
        rows.push(section_header("Serial:"));
        rows.extend_from_slice(serial);
    }
    rows
}

fn io_parallel_rows(parallel_device: ParallelDevice) -> Vec<Row> {
    let mut rows = vec![section_header("Parallel:")];
    rows.extend_from_slice(parallel_rows(parallel_device));
    rows
}

fn io_networking_rows() -> Vec<Row> {
    let mut rows = vec![section_header("Ethernet:")];
    rows.extend_from_slice(&ETHERNET_ROWS);
    rows
}

fn io_audio_rows() -> Vec<Row> {
    let mut rows = vec![section_header("Sound Card:")];
    rows.extend_from_slice(&SOUND_ROWS);
    rows
}

/// Serial rows for the current mode. Only the `midi` build has any; without it
/// the Serial section is empty and omitted from the I/O Ports tab.
fn serial_rows(
    serial_mode: SerialMode,
    midi_out_is_mt32: bool,
    midi_out_is_csynth: bool,
) -> &'static [Row] {
    #[cfg(feature = "midi")]
    {
        if serial_mode != SerialMode::Midi {
            return match serial_mode {
                SerialMode::TcpConnect => &SERIAL_ROWS_TCP_CONNECT,
                SerialMode::Tcp => &SERIAL_ROWS_TCP_LISTEN,
                _ => &SERIAL_ROWS_BASE,
            };
        }
        #[cfg(feature = "mt32")]
        if midi_out_is_mt32 {
            return &SERIAL_ROWS_MT32;
        }
        #[cfg(feature = "coppersynth")]
        if midi_out_is_csynth {
            return &SERIAL_ROWS_CSYNTH;
        }
        let _ = (midi_out_is_mt32, midi_out_is_csynth);
        &SERIAL_ROWS_MIDI
    }
    #[cfg(not(feature = "midi"))]
    {
        let _ = (serial_mode, midi_out_is_mt32, midi_out_is_csynth);
        &[]
    }
}

/// Parallel rows for the selected device: the printer adds its output-file
/// picker, the sampler its input and gain; None shows just the Device selector.
fn parallel_rows(parallel_device: ParallelDevice) -> &'static [Row] {
    match parallel_device {
        ParallelDevice::Sampler => &PARALLEL_ROWS_SAMPLER,
        ParallelDevice::Printer => &PARALLEL_ROWS_PRINTER,
        ParallelDevice::None => &PARALLEL_ROWS_BASE,
    }
}

/// Machine models offered in the selector strip, roughly chronological.
pub const MODELS: [MachineModel; 10] = [
    MachineModel::A1000,
    MachineModel::A500Ocs,
    MachineModel::A500,
    MachineModel::A500Plus,
    MachineModel::A600,
    MachineModel::A1200,
    MachineModel::A3000,
    MachineModel::A4000,
    MachineModel::Cdtv,
    MachineModel::Cd32,
];

// --- value preset lists for the cycle/stepper controls -------------------

const CHIPSETS: [Chipset; 3] = [Chipset::Ocs, Chipset::Ecs, Chipset::Aga];
const RTG_CARDS: [RtgCard; 6] = [
    RtgCard::None,
    RtgCard::Picasso2,
    RtgCard::Picasso2Plus,
    RtgCard::GraffityZ2,
    RtgCard::GraffityZ3,
    RtgCard::Z3660,
];
const AGNUS_CHOICES: [Option<AgnusRevision>; 5] = [
    None,
    Some(AgnusRevision::Ocs),
    Some(AgnusRevision::Ecs8372Rev4),
    Some(AgnusRevision::Ecs8375),
    Some(AgnusRevision::AgaAlice),
];
const DENISE_CHOICES: [Option<DeniseRevision>; 4] = [
    None,
    Some(DeniseRevision::Ocs),
    Some(DeniseRevision::Ecs8373),
    Some(DeniseRevision::AgaLisa),
];
const VIDEO_CHOICES: [VideoStandard; 2] = [VideoStandard::Pal, VideoStandard::Ntsc];
const CPUS: [CpuModel; 7] = [
    CpuModel::M68000,
    CpuModel::M68010,
    CpuModel::M68EC020,
    CpuModel::M68020,
    CpuModel::M68030,
    CpuModel::M68040,
    CpuModel::M68060,
];
const CLOCK_PRESETS: [f64; 10] = [
    7.09, 14.0, 14.18, 25.0, 28.0, 33.0, 40.0, 50.0, 100.0, 200.0,
];
const CHIP_PRESETS: [usize; 4] = [256 * 1024, 512 * 1024, 1024 * 1024, 2 * 1024 * 1024];
const FAST_PRESETS: [usize; 9] = [
    0,
    64 * 1024,
    128 * 1024,
    256 * 1024,
    512 * 1024,
    1024 * 1024,
    2 * 1024 * 1024,
    4 * 1024 * 1024,
    8 * 1024 * 1024,
];
const SLOW_PRESETS: [usize; 3] = [0, 256 * 1024, 512 * 1024];
/// Ramsey bank fills: 1M-4M on 256Kx4 parts, then whole 4M banks of 1Mx4.
const MB_PRESETS: [usize; 8] = [
    0,
    1024 * 1024,
    2 * 1024 * 1024,
    3 * 1024 * 1024,
    4 * 1024 * 1024,
    8 * 1024 * 1024,
    12 * 1024 * 1024,
    16 * 1024 * 1024,
];
/// The A4000 additionally fills the $04000000-$06FFFFFF motherboard RAM
/// expansion space beyond Ramsey's four banks.
const MB_PRESETS_A4000: [usize; 10] = [
    0,
    1024 * 1024,
    2 * 1024 * 1024,
    3 * 1024 * 1024,
    4 * 1024 * 1024,
    8 * 1024 * 1024,
    12 * 1024 * 1024,
    16 * 1024 * 1024,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
];
/// CPU-slot accelerator RAM at $08000000: whatever the CPU board carries,
/// up to the whole 128M coprocessor-slot space.
const ACCEL_PRESETS: [usize; 5] = [
    0,
    16 * 1024 * 1024,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
    128 * 1024 * 1024,
];
const Z3_PRESETS: [usize; 8] = [
    0,
    16 * 1024 * 1024,
    32 * 1024 * 1024,
    64 * 1024 * 1024,
    128 * 1024 * 1024,
    256 * 1024 * 1024,
    512 * 1024 * 1024,
    1024 * 1024 * 1024,
];
const OVERSCANS: [Overscan; 2] = [Overscan::Tv, Overscan::Full];
const PIXEL_ASPECTS: [PixelAspect; 2] = [PixelAspect::Tv, PixelAspect::Square];
const TINTS: [Tint; 5] = [Tint::None, Tint::Bw, Tint::Green, Tint::Amber, Tint::Sepia];
/// The real disks a loaded configuration already gives the machine.
fn raw_host_disks(raw: &RawConfig) -> Vec<crate::config::HostDiskConfig> {
    raw.host_disk
        .iter()
        .filter(|entry| !entry.device.trim().is_empty())
        .map(|entry| crate::config::HostDiskConfig {
            device: entry.device.trim().to_string(),
            fingerprint: entry.fingerprint.clone(),
            identity_confirmed: false,
            attach: entry
                .attach
                .as_deref()
                .map(str::trim)
                .and_then(|token| {
                    crate::config::HostDiskAttach::all()
                        .iter()
                        .copied()
                        .find(|a| a.token().eq_ignore_ascii_case(token))
                })
                .unwrap_or_default(),
            writable: !entry.read_only.unwrap_or(true),
        })
        .collect()
}

/// One line of the Host Disk table.
///
/// Flattened for drawing: the page shows text, and the work of deciding what
/// a device is called and whether it may be touched belongs to the layer that
/// knows about hardware, not to the launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDiskRow {
    /// The identifier the host and the configuration both use: `disk4`,
    /// `sdb`, `PhysicalDrive1`.
    pub id: String,
    /// Opaque hardware identity, when this is a real enumerated disk.
    pub fingerprint: Option<String>,
    /// What the hardware calls itself, or the volume on it.
    pub volume: String,
    /// Capacity, already rounded for reading.
    pub size: String,
    /// Where the host currently has this disk mounted, if anywhere. Shown so
    /// somebody can see why a disk cannot simply be taken.
    pub mounted: Vec<String>,
    /// Whether the guest may write to it. Off by default; writable real-media
    /// access is an explicit choice.
    pub writable: bool,
    /// Where the machine would see this disk -- and only while it is ticked.
    /// An unticked disk is going nowhere, and its cell reads blank rather
    /// than naming a place nothing has claimed.
    pub attach: Option<crate::config::HostDiskAttach>,
}

/// The disks the Host Disk page will offer.
///
/// Every whole device the host has, less the one it is running from. A drive
/// on a SATA port is as usable as one in a card reader, and the emulator is
/// in no position to say which somebody meant -- so an internal disk is
/// labelled rather than withheld, and the refusal is kept for the case that
/// is never right. `blockdev` is what decides that, following a synthesized
/// root (an APFS container, an LVM volume) down to the hardware it lives on.
#[cfg(not(target_arch = "wasm32"))]
fn sample_host_disks() -> Vec<HostDiskRow> {
    crate::blockdev::list_devices()
        .unwrap_or_default()
        .into_iter()
        .filter(|device| device.safety.listable())
        .map(|device| HostDiskRow {
            fingerprint: Some(device.fingerprint()),
            volume: device.model.clone().unwrap_or_else(|| device.id.clone()),
            size: device.size_label(),
            mounted: device.mounted.clone(),
            id: device.id,
            writable: false,
            attach: None,
        })
        .chain(fake_host_disks())
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn sample_host_disks() -> Vec<HostDiskRow> {
    Vec::new()
}

/// Invented disks appended to the real ones, for seeing how a long list
/// behaves without owning a drawer of card readers. `COPPERLINE_FAKE_DISKS=20`
/// adds twenty; unset, nothing changes. Diagnostic only -- they cannot be
/// opened, so mounting one fails as any absent disk does.
fn fake_host_disks() -> Vec<HostDiskRow> {
    let Some(count) = crate::envcfg::var_os("COPPERLINE_FAKE_DISKS") else {
        return Vec::new();
    };
    let count: usize = count.to_string_lossy().trim().parse().unwrap_or(0);
    (0..count.min(64))
        .map(|i| HostDiskRow {
            id: format!("fakedisk{i}"),
            fingerprint: None,
            volume: format!("Pretend Media {i}"),
            size: format!("{}.0 GB", i % 9 + 1),
            mounted: Vec::new(),
            writable: false,
            attach: None,
        })
        .collect()
}

/// How close a bay is to actually having a physical drive behind it.
///
/// The library is compiled in, so the only thing that can be missing is the
/// hardware itself; the launcher says so plainly rather than "None".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeStatus {
    /// Nothing the bridge recognises is plugged in. The bridge itself is
    /// always there in a build with the feature, and a build without one never
    /// shows a bay that could ask.
    NoInterface,
    /// An interface the bridge recognises is attached.
    Attached,
}

/// Every serial device the host offers beyond the library's own scan. macOS:
/// the callout (`cu.*`) devices -- the class made for originating
/// connections, and the one that still names chips the scan's `tty.usb*`
/// filter misses (an Arduino clone on a CH340 mounts as `tty.wchusbserial*`).
/// Linux: the USB serial classes. Windows: nothing extra -- the library
/// already walks every COM port through SetupAPI.
#[cfg(feature = "fluxbridge")]
fn host_serial_ports() -> Vec<String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let Ok(dir) = std::fs::read_dir("/dev") else {
            return Vec::new();
        };
        let wanted = |name: &str| {
            if cfg!(target_os = "macos") {
                name.starts_with("cu.")
            } else {
                name.starts_with("ttyACM") || name.starts_with("ttyUSB")
            }
        };
        let mut ports: Vec<String> = dir
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| wanted(n))
            .map(|n| format!("/dev/{n}"))
            .collect();
        ports.sort();
        ports
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// "Automatic", then the library's scan in its own order, then the host
/// devices it did not name.
///
/// Only the FluxBridge page has a port to choose, so without the feature
/// there is nothing to merge. A macOS device counts as already named when the
/// scan lists its `tty.` twin: `cu.usbmodem101` and `tty.usbmodem101` are
/// one port, and the scan's spelling wins.
#[cfg(feature = "fluxbridge")]
fn merge_port_lists(library: Vec<String>, host: Vec<String>) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = std::iter::once(None)
        .chain(library.iter().cloned().map(Some))
        .collect();
    for port in host {
        let twin = port
            .strip_prefix("/dev/cu.")
            .map(|rest| format!("/dev/tty.{rest}"));
        let named = library
            .iter()
            .any(|l| *l == port || twin.as_deref() == Some(l.as_str()));
        if !named {
            out.push(Some(port));
        }
    }
    out
}

/// The ports the bridge page offers: what the library scanned, plus whatever
/// else the host has.
#[cfg(feature = "fluxbridge")]
fn sample_bridge_ports() -> Vec<Option<String>> {
    merge_port_lists(crate::fluxbridge::com_ports(), host_serial_ports())
}

/// What the bridge can see of the host right now.
fn bridge_status() -> BridgeStatus {
    #[cfg(feature = "fluxbridge")]
    if crate::fluxbridge::interface_connected() {
        return BridgeStatus::Attached;
    }
    BridgeStatus::NoInterface
}

/// Config-file spellings for the bridge settings, matching what the parser
/// accepts.
fn bridge_driver_name(d: BridgeDriver) -> &'static str {
    match d {
        BridgeDriver::DrawBridge => "drawbridge",
        BridgeDriver::Greaseweazle => "greaseweazle",
        BridgeDriver::SupercardPro => "supercardpro",
    }
}

fn bridge_cable_name(c: BridgeCable) -> &'static str {
    match c {
        BridgeCable::DriveA => "a",
        BridgeCable::DriveB => "b",
        BridgeCable::Shugart0 => "0",
        BridgeCable::Shugart1 => "1",
        BridgeCable::Shugart2 => "2",
        BridgeCable::Shugart3 => "3",
    }
}

fn bridge_density_name(d: BridgeDensity) -> &'static str {
    match d {
        BridgeDensity::Auto => "auto",
        BridgeDensity::Dd => "dd",
        BridgeDensity::Hd => "hd",
    }
}

fn bridge_mode_name(m: BridgeReadMode) -> &'static str {
    match m {
        BridgeReadMode::Compatible => "compatible",
        BridgeReadMode::Normal => "normal",
        BridgeReadMode::Stalling => "stalling",
    }
}

/// The interfaces the Interface row offers, straight from the FluxBridge
/// build: the library's own driver table decides what exists, in its own
/// order, so compiling a driver in is the whole job of adding it here.
#[cfg(feature = "fluxbridge")]
fn bridge_drivers() -> Vec<BridgeDriver> {
    crate::fluxbridge::drivers()
        .iter()
        .filter_map(|driver| {
            [
                BridgeDriver::DrawBridge,
                BridgeDriver::Greaseweazle,
                BridgeDriver::SupercardPro,
            ]
            .into_iter()
            .find(|kind| kind.match_token() == driver.token)
        })
        .collect()
}

/// Without the bridge the page is unreachable, but the row still needs a
/// value to show.
#[cfg(not(feature = "fluxbridge"))]
fn bridge_drivers() -> Vec<BridgeDriver> {
    vec![BridgeDriver::default()]
}
const BRIDGE_CABLES: [BridgeCable; 6] = [
    BridgeCable::DriveA,
    BridgeCable::DriveB,
    BridgeCable::Shugart0,
    BridgeCable::Shugart1,
    BridgeCable::Shugart2,
    BridgeCable::Shugart3,
];
const BRIDGE_DENSITIES: [BridgeDensity; 3] =
    [BridgeDensity::Auto, BridgeDensity::Dd, BridgeDensity::Hd];
// The driver's fourth mode, Turbo, is absent: it answers AmigaDOS calls
// instead of reading the disk, so there is nothing for a drive model to do
// with it.
const BRIDGE_READ_MODES: [BridgeReadMode; 3] = [
    BridgeReadMode::Normal,
    BridgeReadMode::Compatible,
    BridgeReadMode::Stalling,
];
const AUDIO_FILTER_MODES: [AudioFilterMode; 3] = [
    AudioFilterMode::Auto,
    AudioFilterMode::On,
    AudioFilterMode::Off,
];
const FLOPPY_SPEEDS: [u16; 5] = [100, 200, 400, 800, crate::floppy::SPEED_TURBO];
// The bridge speed row cycles the same set the config and CLI accept.
use crate::config::SUPPORTED_BRIDGE_SPEED_PERCENTS as BRIDGE_REPLAY_SPEEDS;
const PACINGS: [PacingBudget; 2] = [PacingBudget::Cycles, PacingBudget::Instructions];
const WARPS: [WarpSpeed; 5] = [
    WarpSpeed::X2,
    WarpSpeed::X4,
    WarpSpeed::X8,
    WarpSpeed::X16,
    WarpSpeed::Max,
];
// The stepper flips the two explicit modes, matching the runtime toggle.
const JOYSTICK_MODES: [JoystickInputMode; 2] =
    [JoystickInputMode::Gamepad, JoystickInputMode::Keyboard];
// When the host mouse is grabbed, in stepper order.
const MOUSE_CAPTURES: [MouseCapture; 3] = [
    MouseCapture::Click,
    MouseCapture::Auto,
    MouseCapture::Manual,
];
// Controller devices a game port accepts, in stepper order.
const PORT_DEVICES: [PortDevice; 5] = [
    PortDevice::Mouse,
    PortDevice::Joystick,
    PortDevice::Cd32Pad,
    PortDevice::Analogue,
    PortDevice::None,
];
// `None` = no SCSI board fitted; the two boards are mutually exclusive here even
// though the engine could run both, so a config round-trips through this picker.
const SCSI_CONTROLLERS: [Option<ScsiController>; 4] = [
    None,
    Some(ScsiController::A2091),
    Some(ScsiController::A4091),
    Some(ScsiController::A3000),
];
// `None` = no lide board fitted. Unlike SCSI's boards, all three personalities
// work on any machine model, so there is no per-model filtering to do here.
const LIDE_BOARDS: [Option<LidePersonality>; 4] = [
    None,
    Some(LidePersonality::Ripple),
    Some(LidePersonality::Ride),
    Some(LidePersonality::AtBus2008),
];
#[cfg(feature = "midi")]
const SERIAL_MODES: [SerialMode; 6] = [
    SerialMode::Off,
    SerialMode::Stdout,
    SerialMode::Midi,
    SerialMode::Tcp,
    SerialMode::TcpConnect,
    SerialMode::Pty,
];
/// Stereo-separation presets the picker steps through (percent), ascending so
/// the right arrow steps up (wrapping 100 -> 0) and the left arrow steps down.
/// The config/CLI accept any 0-100; an off-grid value snaps to the nearest here.
const STEREO_SEPARATION_STEPS: [usize; 11] = [0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100];

/// Sampler input-gain presets the picker steps through (preamp decibels, in
/// 3 dB steps), ascending. 0 dB is unity; the ends are the sampler's
/// [`crate::sampler::MIN_SAMPLER_GAIN_DB`]..[`crate::sampler::MAX_SAMPLER_GAIN_DB`].
/// The config/CLI accept any value in range; an off-grid value snaps to the
/// nearest here.
const SAMPLER_GAIN_STEPS: [f64; 17] = [
    -24.0, -21.0, -18.0, -15.0, -12.0, -9.0, -6.0, -3.0, 0.0, 3.0, 6.0, 9.0, 12.0, 15.0, 18.0,
    21.0, 24.0,
];

/// Label a sampler gain in decibels, e.g. `0 dB`, `+6 dB`, `-12 dB`.
fn sampler_gain_label(gain_db: f32) -> String {
    if gain_db.abs() < 0.05 {
        "0 dB".to_string()
    } else {
        format!("{gain_db:+.0} dB")
    }
}

/// A fully-typed, editable mirror of a configurable machine. See the module
/// docs for how it round-trips through [`RawConfig`].
#[derive(Debug, Clone)]
pub struct MachineSetup {
    /// Selected machine profile (`None` is the no-profile default, equivalent
    /// to the `A500`).
    model: Option<MachineModel>,
    // System
    chipset: Chipset,
    /// Explicit Agnus override; `None` derives from the chipset/profile.
    agnus: Option<AgnusRevision>,
    /// Explicit Denise override; `None` derives from the chipset/profile.
    denise: Option<DeniseRevision>,
    video: VideoStandard,
    rtc: bool,
    identify: bool,
    rtg: RtgCard,
    /// Memory fitted to a configurable RTG board. The launcher currently
    /// preserves this value from loaded configs; the card selector does not
    /// need a separate control for the two period-correct Picasso II/II+ sizes.
    rtg_vram_bytes: usize,
    // CPU
    cpu: CpuModel,
    fpu: bool,
    clock_mhz: f64,
    icache: bool,
    dcache: bool,
    /// Fast batch/trace-JIT CPU execution (`[cpu] jit`); not cycle-exact.
    jit: bool,
    // Memory (bytes)
    chip_ram: usize,
    fast_ram: usize,
    slow_ram: usize,
    /// Diagnostic cold-start policy selected by the Memory page.
    ram_init: RamInit,
    /// Values remembered while another fill mode is selected, so cycling away
    /// from a loaded/custom mode and back does not silently replace its value.
    ram_pattern: u16,
    ram_random_seed: u64,
    mb_ram: usize,
    accel_ram: usize,
    z3_ram: usize,
    // ROM (None = bundled AROS for the boot ROM, none for extended)
    rom: Option<PathBuf>,
    extended_rom: Option<PathBuf>,
    // Floppy
    floppy_drives: u8,
    /// `[floppy] speed`: a percentage (100/200/400/800) or 0 for turbo.
    floppy_speed: u16,
    /// Per-drive disk-swap playlists (entry 0 is the boot disk). A single
    /// image is a one-element list.
    df_playlists: [Vec<PathBuf>; 4],
    df_write_protected: [bool; 4],
    /// A real drive on this bay instead of an image. `None` is the ordinary
    /// image-backed drive.
    df_bridge: [Option<FluxBridgeConfig>; 4],
    /// The bay's interface is set to "None": still a physical-drive bay in
    /// the launcher, but with no interface to drive it -- the run and the
    /// written config treat it as unbridged. Selected automatically when a
    /// bay is bridged with nothing attached.
    df_bridge_none: [bool; 4],
    /// Which bay the FluxBridge settings page is showing. The page itself is
    /// one set of rows; this says whose values they are.
    bridge_edit_drive: usize,
    /// What the library could see the last time we looked. Sampled when the
    /// launcher opens and whenever a bay is switched over to a physical drive,
    /// so a board plugged in mid-session is picked up by unticking and
    /// re-ticking the box rather than by a scan every frame.
    bridge_status: BridgeStatus,
    /// The serial ports on offer -- "Automatic", the library's scan, then
    /// every host serial device the scan did not name. Sampled with
    /// `bridge_status` (launcher open, a bay switched to a physical drive):
    /// the scan walks the serial bus, which is not a per-frame activity.
    #[cfg(feature = "fluxbridge")]
    bridge_ports: Vec<Option<String>>,
    /// Host disks the Host Disk page can offer, sampled when that page is
    /// opened or refreshed rather than every frame: enumerating walks the
    /// host's storage tree, and a card pushed in mid-session is picked up by
    /// Refresh rather than by polling.
    host_disks: Vec<HostDiskRow>,
    /// First row shown in the disk table. The list can be longer than the
    /// box, and a disk that cannot be scrolled to cannot be chosen.
    host_disk_scroll: usize,
    /// How fast a held scroll over that table is running.
    host_disk_scroll_rate: ScrollRate,
    /// Why the last tick was refused. Read once by the caller, which puts it
    /// on the status line with every other warning; cleared by the next tick,
    /// because the situation it describes is the one being changed.
    host_disk_warning: Option<String>,
    /// The disks ticked in the table. A machine can take several at once, so
    /// long as no two want the same place; ticking one claims the first place
    /// still free. Seeded from what is already attached, so leaving the page
    /// and coming back shows the machine as it stands rather than as new.
    host_disk_selected: Vec<String>,
    /// Real disks given to the machine, at most one per attachment point.
    /// Held in the configuration's own shape: this is a setting that
    /// persists, not a fact about this session.
    host_disks_attached: Vec<crate::config::HostDiskConfig>,
    // Hard disk. Each drive's optional volume-name override (directory mounts
    // only) sits in the matching `*_name` slot, paralleling the path slot. Boot
    // priority is edited on the Boot Priority sub-page and split across two
    // slots: `*_bootpri` holds the priority (None = unset, shown as 0), and
    // `*_boot_off` is the Bootable checkbox cleared -- the config's -128 (never)
    // sentinel. They are separate so unticking Bootable greys the priority
    // without discarding the number within the session (the config can only
    // store one or the other, so a save of a disabled drive still writes -128).
    ide_master: Option<PathBuf>,
    ide_master_name: Option<String>,
    /// The filesystem an in-memory directory-mount volume is built with
    /// (meaningless, and never emitted, unless the path is a directory);
    /// paralleling `*_name` above.
    ide_master_fs: crate::diskimage::FileSystem,
    /// Whether `ide_master`'s path is a host directory, sampled whenever
    /// the path is set or loaded rather than on every frame the row is
    /// drawn: `Path::is_dir()` is a host filesystem call that can stall on
    /// a slow or disconnected mount, and the FFS/OFS toggle's visibility
    /// (`launcher_drive_fs_applies`, `ui.rs`) needs it every frame that row
    /// is on screen.
    ide_master_is_dir: bool,
    ide_master_bootpri: Option<i8>,
    ide_master_boot_off: bool,
    ide_slave: Option<PathBuf>,
    ide_slave_name: Option<String>,
    ide_slave_fs: crate::diskimage::FileSystem,
    ide_slave_is_dir: bool,
    ide_slave_bootpri: Option<i8>,
    ide_slave_boot_off: bool,
    /// Which SCSI host adapter is fitted, or `None` for no board. Shares the
    /// `scsi_*` ROM/unit block below (the drives are portable between boards).
    scsi_controller: Option<ScsiController>,
    scsi_rom: Option<PathBuf>,
    scsi_rom_odd: Option<PathBuf>,
    scsi_units: [Option<PathBuf>; 7],
    scsi_unit_names: [Option<String>; 7],
    scsi_unit_fs: [crate::diskimage::FileSystem; 7],
    /// Paralleling `ide_master_is_dir`, per unit.
    scsi_unit_is_dir: [bool; 7],
    scsi_unit_bootpri: [Option<i8>; 7],
    scsi_unit_boot_off: [bool; 7],
    /// Which lide personality is fitted, or `None` for no board. Unlike
    /// `[scsi]`, presence is inferred from the config's own `rom`/`drives`
    /// (see `LideConfig::enabled`); this `Option` is purely the launcher's
    /// session-level "is a board fitted" toggle.
    lide_board: Option<LidePersonality>,
    lide_rom: Option<PathBuf>,
    lide_rom_bank2: Option<PathBuf>,
    /// Drive slots in (channel, master/slave) order (RIPPLE's two channels;
    /// RIDE/AT-Bus 2008 only ever use the first two). `[lide] drives` is a
    /// positional list in the config file -- a hole cannot be represented --
    /// so `clear_path` cascades: clearing slot N also clears every slot
    /// after it, keeping this array always representable as a config.
    lide_drives: [Option<PathBuf>; 4],
    lide_drive_names: [Option<String>; 4],
    lide_drive_fs: [crate::diskimage::FileSystem; 4],
    /// Paralleling `ide_master_is_dir`, per drive.
    lide_drive_is_dir: [bool; 4],
    lide_drive_bootpri: [Option<i8>; 4],
    lide_drive_boot_off: [bool; 4],
    // Host FS mounts. The GUI edits the first FILESYS_GUI_SLOTS entries
    // (directory + optional volume name + boot priority, -128 = never boot);
    // any further hand-written [[filesys]] entries are carried in
    // `filesys_extra` and re-emitted verbatim so a save never drops them.
    filesys_dirs: [Option<PathBuf>; FILESYS_GUI_SLOTS],
    filesys_names: [Option<String>; FILESYS_GUI_SLOTS],
    filesys_bootpri: [i8; FILESYS_GUI_SLOTS],
    filesys_readonly: [bool; FILESYS_GUI_SLOTS],
    filesys_extra: Vec<RawFilesysMount>,
    // CD
    cd_image: Option<PathBuf>,
    cd_insert_delay: f64,
    cd32_nvram: Option<PathBuf>,
    // WHDLoad direct boot (`[whdload]`, src/whdload.rs): the game package
    // and the staging directories, edited on the Storage tab's WHDLoad
    // sub-page. `args` has no row of its own; it is carried through so a
    // hand-written key survives a launcher save.
    whdload_game: Option<PathBuf>,
    whdload_kickstarts: Option<PathBuf>,
    whdload_library: Option<PathBuf>,
    whdload_args: Option<String>,
    /// The WHDLoad distribution and Soft-Kicker archives, when they are not
    /// the copies the release ships with.
    whdload_whd_package: Option<PathBuf>,
    whdload_skick_package: Option<PathBuf>,
    /// Which machine a package boots on.
    whdload_machine: crate::config::WhdloadMachine,
    /// Where the game database lives, and whether WHDLoad has a link of its
    /// own in the left-hand navigation. Both belong to the library, so a
    /// build without it carries them through a save untouched rather than
    /// editing or dropping them.
    /// Where the Library page keeps its scanned library and its downloads.
    /// Configuration-file only -- see `RawWhdload::library_db` -- so they
    /// are held here rather than being launcher fields with rows.
    whdload_library_db: Option<PathBuf>,
    whdload_library_cache: Option<PathBuf>,
    whdload_enabled: bool,
    /// A directory of packages, which the Library page lists.
    whdload_games: Option<PathBuf>,
    // Serial port. Carried in every build so a config's `[serial]` block
    // round-trips; only edited in the I/O Ports tab's Serial section, which a
    // `midi` build shows.
    serial_mode: SerialMode,
    midi_out: Option<String>,
    midi_in: Option<String>,
    /// TCP listen address for `mode = "tcp"`, typed into the Listen box the
    /// Serial section shows in that mode. `None` binds the default
    /// ([`crate::config::SERIAL_TCP_DEFAULT_LISTEN`]).
    serial_listen: Option<String>,
    /// Dial-out address for `mode = "tcp-connect"`, typed into the Connect
    /// box that mode shows. `None` there has nothing to dial, and the run
    /// says so rather than the launcher refusing the mode.
    serial_connect: Option<String>,
    /// The Centronics parallel-port device (None/Printer/Sampler), edited in the
    /// I/O Ports tab's Parallel section.
    parallel_device: crate::config::ParallelDevice,
    /// The printer capture path, edited by the Output file row (shown when the
    /// Printer device is selected) and carried through from a hand-written
    /// `[parallel] output`.
    parallel_output: Option<PathBuf>,
    /// Sampler host capture device (`None` = system default) and its input gain,
    /// edited in the I/O Ports tab's Parallel section.
    sampler_input: Option<String>,
    sampler_gain_db: f32,
    /// The A2065 Ethernet board, edited in the I/O Ports tab's Ethernet
    /// section: `None` = not fitted, `Some(backend)` fits the board with that
    /// host backend (`NetConfig::None` = fitted but isolated).
    a2065_net: Option<NetConfig>,
    /// The bundled HostSocket bsdsocket.library board, edited in the same
    /// Ethernet section: `None` = not fitted, `Some(backend)` fits it.
    hostsocket_net: Option<NetConfig>,
    /// Whether the HostSocket board is in `net = "host"` mode (real host OS
    /// sockets via direct passthrough, `crate::hostsocket`'s own doc
    /// comment) -- not a `NetConfig` backend at all, so it can't live in
    /// `hostsocket_net` the way the other choices do; overrides it for
    /// display and saving when set (see `Config::hostsocket_transport`'s
    /// own comment for why this needs a field of its own).
    hostsocket_host_mode: bool,
    /// `[hostsocket]` keys the launcher does not edit (DNS resolver
    /// address/strategy, guest hostname, and the bridge-only interface
    /// address/gateway), carried through so saving a config keeps them.
    hostsocket_dns_server: Option<String>,
    hostsocket_hostname: Option<String>,
    hostsocket_address: Option<String>,
    hostsocket_gateway: Option<String>,
    hostsocket_resolver: Option<String>,
    /// The MacroSystem Toccata sound board, edited in the I/O Ports tab's
    /// Audio page (`[toccata] enabled`). No other options exist yet.
    toccata: bool,
    /// The MHI virtual MPEG audio decoder board, edited on the same Audio
    /// page (`[mhi] enabled`) in an `mhi` build, the only build that can
    /// fit the board. Kept as an unconditional passthrough field even in a
    /// non-`mhi` build so loading and re-saving a config does not silently
    /// drop a `[mhi] enabled` set by some other build -- only the launcher
    /// row/toggle that edits it is feature-gated.
    mhi: bool,
    /// Currently visible host bridge adapters: stable identifier + label.
    bridge_interfaces: Vec<(String, String)>,
    /// Input device names for the sampler picker: filled when the screen opens
    /// and re-read each time the field is cycled, so a reconnected device
    /// appears.
    sampler_input_devices: Vec<String>,
    /// Host endpoints for the device pickers, read once when this setup is
    /// built so a fresh config screen sees currently-connected devices.
    #[cfg(feature = "midi")]
    midi_endpoints: crate::midi::MidiEndpoints,
    // A/V and emulation
    /// Host audio output selection: system default, a named device, or Disabled
    /// (no sound). Carried in every build so `[audio]` round-trips.
    audio_output: crate::audio::AudioOutput,
    /// Output device names for the picker: filled when the screen opens and
    /// re-read each time the field is cycled, so a reconnected device appears.
    audio_devices: Vec<String>,
    /// Stereo (hardware panning) or mono (L/R averaged).
    audio_channel_mode: ChannelMode,
    /// Stereo width, 0-100 (100 = full hardware panning).
    audio_stereo_separation: u8,
    /// Paula analogue filter override: Auto (guest-driven), On, or Off.
    audio_filter: AudioFilterMode,
    /// `[audio] stem_granularity`: the headless `--audio-stems-mode` default.
    /// No launcher row edits it (stem capture is a headless-only flag), but a
    /// loaded config's value must survive a Save, so it is carried through
    /// as a passthrough rather than silently dropped by `to_raw`.
    audio_stem_granularity: Option<Vec<crate::audio::mux::StemGranularity>>,
    overscan: Overscan,
    pixel_aspect: PixelAspect,
    /// How the canvas is scaled into the window ([display] scaling).
    scaling: DisplayScaling,
    /// Motion-adaptive interlace weaving ([display] deinterlace).
    deinterlace: bool,
    phosphor: f32,
    /// Window shader pass ([display] shader).
    shader: ShaderMode,
    /// The user shader the loaded config named, kept even while another
    /// preset is selected so the picker can offer it again. A file the
    /// config never mentioned cannot be reached from here: the field takes
    /// a path, and this screen has no shader browser.
    shader_custom: Option<PathBuf>,
    /// Shader mix, 0.0 to 1.0 ([display] shader_strength).
    shader_strength: f32,
    /// Which monitor front frames the picture, if any ([display] bezel).
    bezel: BezelStyle,
    /// Folder of PNG stickers drawn onto the bezel ([display]
    /// bezel_stickers). No launcher row edits it -- like the custom
    /// shader, there is no file browser -- but it is carried so a
    /// configured folder survives the launcher's config round-trip.
    bezel_stickers: Option<PathBuf>,
    /// Performance overlay in the top-right ([display] perf_overlay).
    perf_overlay: bool,
    /// The MT-32's two ROM images, whether its front panel starts up, and
    /// how that panel's display is lit.
    mt32_control_rom: Option<PathBuf>,
    mt32_pcm_rom: Option<PathBuf>,
    mt32_panel: bool,
    mt32_lcd: Mt32Lcd,
    /// Coppersynth's soundfont and translation mode ([serial] coppersynth_*).
    csynth_soundfont: Option<PathBuf>,
    csynth_mt32_mode: Option<String>,
    csynth_panel: bool,
    /// How large the pop-up menu is drawn ([display] menu_scale).
    menu_scale: MenuScale,
    /// Screen tint ([display] tint).
    tint: Tint,
    /// Open fullscreen at start ([display] full_screen).
    start_fullscreen: bool,
    /// Show the status bar at start ([display] status_bar).
    show_status_bar: bool,
    floppy_sounds: bool,
    floppy_volume: u8,
    power_on: bool,
    pacing_budget: PacingBudget,
    realtime_priority: bool,
    warp: WarpSpeed,
    joystick_input_mode: JoystickInputMode,
    mouse_sensitivity: u8,
    mouse_capture: MouseCapture,
    port_devices: [PortDevice; 2],
    // Extra Zorro boards (metadata path + plugin config schema/overrides)
    zorro_boards: Vec<ZorroBoardSetup>,
    /// The Paths page: the configuration's `[paths]` section, saved with
    /// the rest of it. Empty until somebody moves something, so a
    /// configuration that never mentions directories stays as portable as
    /// it was -- and one that does still starts on a machine that has
    /// never seen them, because what cannot be reached is dropped when the
    /// configuration is adopted.
    paths: crate::pathconf::Paths,
}

impl Default for MachineSetup {
    fn default() -> Self {
        // The empty raw config is always valid (the built-in defaults).
        Self::from_raw(&RawConfig::default()).expect("default config is valid")
    }
}

impl MachineSetup {
    /// Build the typed model from a raw config, validating it through the
    /// config pipeline first. The validated [`Config`] supplies the resolved
    /// scalar values; the raw view supplies the things `Config` does not
    /// preserve: whether the Agnus/Denise were explicit overrides, the
    /// "no boot ROM = AROS" distinction, and the `[[zorro]]` board paths.
    pub fn from_raw(raw: &RawConfig) -> Result<Self> {
        let cfg: Config = raw.clone().try_into()?;
        // One tick box governs both kinds of bay, so read it from whichever
        // of the two a bay actually has.
        let df_write_protected = std::array::from_fn(|i| {
            cfg.floppy.drives[i]
                .as_ref()
                .map(|d| d.write_protected)
                .or_else(|| cfg.floppy.bridges[i].as_ref().map(|b| b.write_protected))
                .unwrap_or(true)
        });
        let connected = cfg.floppy_connected.iter().filter(|&&c| c).count().max(1) as u8;
        Ok(Self {
            model: cfg.machine,
            chipset: cfg.chipset,
            agnus: raw.chipset.agnus.is_some().then_some(cfg.agnus_revision),
            denise: raw.chipset.denise.is_some().then_some(cfg.denise_revision),
            video: cfg.video_standard,
            rtc: cfg.rtc_present,
            identify: cfg.identify_board,
            rtg: cfg.rtg,
            rtg_vram_bytes: cfg.rtg_vram_bytes,
            cpu: cfg.cpu,
            fpu: cfg.fpu,
            clock_mhz: cfg.cpu_clock_mhz,
            icache: cfg.cpu_icache,
            dcache: cfg.cpu_dcache,
            jit: cfg.cpu_jit,
            chip_ram: cfg.chip_ram_bytes,
            fast_ram: cfg.fast_ram_bytes,
            slow_ram: cfg.slow_ram_bytes,
            ram_init: cfg.ram_init,
            ram_pattern: match cfg.ram_init {
                RamInit::Pattern { word } => word,
                _ => DEFAULT_RAM_PATTERN,
            },
            ram_random_seed: match cfg.ram_init {
                RamInit::Random { seed } => seed,
                _ => DEFAULT_RANDOM_RAM_SEED,
            },
            mb_ram: cfg.mb_ram_bytes,
            accel_ram: cfg.accel_ram_bytes,
            z3_ram: cfg.z3_ram_bytes,
            rom: raw.rom.as_deref().map(PathBuf::from),
            extended_rom: raw.extended_rom.as_deref().map(PathBuf::from),
            floppy_drives: raw.floppy.drives.unwrap_or(connected).clamp(1, 4),
            floppy_speed: cfg.floppy.speed,
            df_playlists: cfg.floppy_playlists.clone(),
            df_write_protected,
            df_bridge: std::array::from_fn(|i| cfg.floppy.bridges[i].clone()),
            // A config that names a bridge names an interface; "None" is a
            // launcher-session state, never read from a file.
            df_bridge_none: [false; 4],
            bridge_edit_drive: 0,
            bridge_status: bridge_status(),
            #[cfg(feature = "fluxbridge")]
            bridge_ports: sample_bridge_ports(),
            // Not sampled at construction: the page samples when it opens, so
            // a launcher that never visits it never touches the host's disks.
            host_disks: Vec::new(),
            host_disk_warning: None,
            host_disk_selected: Vec::new(),
            host_disk_scroll: 0,
            host_disk_scroll_rate: ScrollRate::default(),
            host_disks_attached: raw_host_disks(raw),
            ide_master: cfg.ide.master.as_ref().map(|d| d.path.clone()),
            ide_master_name: cfg.ide.master.as_ref().and_then(|d| d.volume_name.clone()),
            ide_master_fs: cfg
                .ide
                .master
                .as_ref()
                .map(|d| d.filesystem)
                .unwrap_or(crate::diskimage::FileSystem::FFS),
            ide_master_is_dir: cfg.ide.master.as_ref().is_some_and(|d| d.path.is_dir()),
            ide_master_bootpri: boot_priority_of(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_master_boot_off: boot_is_off(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_slave: cfg.ide.slave.as_ref().map(|d| d.path.clone()),
            ide_slave_name: cfg.ide.slave.as_ref().and_then(|d| d.volume_name.clone()),
            ide_slave_fs: cfg
                .ide
                .slave
                .as_ref()
                .map(|d| d.filesystem)
                .unwrap_or(crate::diskimage::FileSystem::FFS),
            ide_slave_is_dir: cfg.ide.slave.as_ref().is_some_and(|d| d.path.is_dir()),
            ide_slave_bootpri: boot_priority_of(raw.ide.slave.as_ref().and_then(|d| d.bootpri)),
            ide_slave_boot_off: boot_is_off(raw.ide.slave.as_ref().and_then(|d| d.bootpri)),
            scsi_controller: cfg.scsi.enabled().then_some(cfg.scsi.controller),
            scsi_rom: cfg.scsi.rom.clone(),
            scsi_rom_odd: cfg.scsi.rom_odd.clone(),
            scsi_units: std::array::from_fn(|i| cfg.scsi.units[i].as_ref().map(|d| d.path.clone())),
            scsi_unit_names: std::array::from_fn(|i| {
                cfg.scsi.units[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
            }),
            scsi_unit_fs: std::array::from_fn(|i| {
                cfg.scsi.units[i]
                    .as_ref()
                    .map(|d| d.filesystem)
                    .unwrap_or(crate::diskimage::FileSystem::FFS)
            }),
            scsi_unit_is_dir: std::array::from_fn(|i| {
                cfg.scsi.units[i].as_ref().is_some_and(|d| d.path.is_dir())
            }),
            scsi_unit_bootpri: std::array::from_fn(|i| {
                boot_priority_of(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
            }),
            scsi_unit_boot_off: std::array::from_fn(|i| {
                boot_is_off(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
            }),
            lide_board: cfg.lide.enabled().then_some(cfg.lide.board),
            lide_rom: cfg.lide.rom.clone(),
            lide_rom_bank2: cfg.lide.rom_bank2.clone(),
            lide_drives: std::array::from_fn(|i| {
                cfg.lide.drives[i].as_ref().map(|d| d.path.clone())
            }),
            lide_drive_names: std::array::from_fn(|i| {
                cfg.lide.drives[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
            }),
            lide_drive_fs: std::array::from_fn(|i| {
                cfg.lide.drives[i]
                    .as_ref()
                    .map(|d| d.filesystem)
                    .unwrap_or(crate::diskimage::FileSystem::FFS)
            }),
            lide_drive_is_dir: std::array::from_fn(|i| {
                cfg.lide.drives[i].as_ref().is_some_and(|d| d.path.is_dir())
            }),
            lide_drive_bootpri: std::array::from_fn(|i| {
                boot_priority_of(raw.lide.drives.get(i).and_then(|d| d.bootpri))
            }),
            lide_drive_boot_off: std::array::from_fn(|i| {
                boot_is_off(raw.lide.drives.get(i).and_then(|d| d.bootpri))
            }),
            filesys_dirs: std::array::from_fn(|i| {
                raw.filesys.get(i).map(|m| PathBuf::from(&m.path))
            }),
            filesys_names: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.volume.clone())
            }),
            filesys_bootpri: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.bootpri).unwrap_or(-128)
            }),
            filesys_readonly: std::array::from_fn(|i| {
                raw.filesys.get(i).and_then(|m| m.readonly).unwrap_or(false)
            }),
            filesys_extra: raw
                .filesys
                .iter()
                .skip(FILESYS_GUI_SLOTS)
                .cloned()
                .collect(),
            cd_image: cfg.cd_image_path.clone(),
            cd_insert_delay: cfg.cd_insert_delay_secs,
            // Use the raw NVRAM path: Config defaults it to "cd32-nvram.bin"
            // on CD32, which we do not want to persist as an explicit setting.
            cd32_nvram: raw.cd.nvram.as_deref().map(PathBuf::from),
            whdload_game: raw.whdload.game.as_deref().map(PathBuf::from),
            whdload_kickstarts: raw.whdload.kickstarts.as_deref().map(PathBuf::from),
            whdload_library: raw.whdload.library.as_deref().map(PathBuf::from),
            whdload_args: raw.whdload.args.clone(),
            whdload_whd_package: raw.whdload.whd_package.as_deref().map(PathBuf::from),
            whdload_skick_package: raw.whdload.skick_package.as_deref().map(PathBuf::from),
            whdload_machine: raw.whdload.machine_type.unwrap_or_default(),
            whdload_library_db: raw.whdload.library_db.as_deref().map(PathBuf::from),
            whdload_library_cache: raw.whdload.library_cache.as_deref().map(PathBuf::from),
            // On unless told otherwise: a fresh installation should find
            // the page there rather than have to be told to show it.
            whdload_enabled: raw.whdload.enabled.unwrap_or(true),
            whdload_games: raw.whdload.games.as_deref().map(PathBuf::from),
            serial_mode: cfg.serial.mode,
            midi_out: cfg.serial.midi_out.clone(),
            midi_in: cfg.serial.midi_in.clone(),
            serial_listen: cfg.serial.listen.clone(),
            serial_connect: cfg.serial.connect.clone(),
            parallel_device: cfg.parallel.device,
            parallel_output: cfg.parallel.printer_output.clone(),
            sampler_input: cfg.parallel.sampler_input.clone(),
            sampler_gain_db: cfg.parallel.sampler_gain_db,
            a2065_net: cfg.a2065_net.clone(),
            hostsocket_net: cfg.hostsocket_net.clone(),
            hostsocket_host_mode: cfg.hostsocket_transport.as_deref() == Some("host"),
            hostsocket_dns_server: raw.hostsocket.dns_server.clone(),
            hostsocket_hostname: raw.hostsocket.hostname.clone(),
            hostsocket_address: raw.hostsocket.address.clone(),
            hostsocket_gateway: raw.hostsocket.gateway.clone(),
            hostsocket_resolver: raw.hostsocket.resolver.clone(),
            toccata: cfg.toccata,
            mhi: cfg.mhi,
            bridge_interfaces: Vec::new(),
            // Filled by refresh_sampler_inputs on open, like the audio devices.
            sampler_input_devices: Vec::new(),
            // Left empty here so config construction stays side-effect free; the
            // config screen fills it via refresh_midi_endpoints on open.
            #[cfg(feature = "midi")]
            midi_endpoints: crate::midi::MidiEndpoints::default(),
            audio_output: crate::audio::AudioOutput::from_config(
                cfg.audio.output_enabled,
                cfg.audio.output_device.as_deref(),
            ),
            // Filled by refresh_audio_devices on open, like the MIDI endpoints.
            audio_devices: Vec::new(),
            audio_channel_mode: cfg.audio.channel_mode,
            audio_stereo_separation: cfg.audio.stereo_separation,
            audio_filter: cfg.audio.filter,
            audio_stem_granularity: cfg.audio.stem_granularity.clone(),
            overscan: cfg.overscan,
            pixel_aspect: cfg.pixel_aspect,
            scaling: cfg.scaling,
            deinterlace: cfg.deinterlace,
            phosphor: cfg.phosphor,
            shader: cfg.shader.clone(),
            shader_custom: match &cfg.shader {
                ShaderMode::Custom(path) => Some(path.clone()),
                _ => None,
            },
            shader_strength: cfg.shader_strength,
            bezel: cfg.bezel,
            bezel_stickers: cfg.bezel_stickers.clone(),
            perf_overlay: cfg.perf_overlay,
            mt32_control_rom: cfg.serial.mt32_control_rom.clone(),
            mt32_pcm_rom: cfg.serial.mt32_pcm_rom.clone(),
            mt32_panel: cfg.serial.mt32_panel,
            mt32_lcd: cfg.serial.mt32_lcd,
            csynth_soundfont: cfg.serial.coppersynth_soundfont.clone(),
            csynth_mt32_mode: cfg.serial.coppersynth_mt32_mode.clone(),
            csynth_panel: cfg.serial.coppersynth_panel,
            menu_scale: cfg.menu_scale,
            tint: cfg.tint,
            start_fullscreen: cfg.full_screen,
            show_status_bar: cfg.status_bar,
            floppy_sounds: cfg.audio.floppy_sounds,
            floppy_volume: cfg.audio.floppy_sounds_volume,
            power_on: cfg.emulation.power_on,
            pacing_budget: cfg.emulation.pacing_budget,
            realtime_priority: cfg.emulation.realtime_priority,
            warp: cfg.emulation.warp_speed,
            joystick_input_mode: cfg.joystick_input_mode,
            mouse_sensitivity: cfg.mouse_sensitivity,
            mouse_capture: cfg.mouse_capture,
            port_devices: cfg.port_devices,
            zorro_boards: raw
                .zorro
                .iter()
                .map(|b| {
                    let mut board = ZorroBoardSetup::load(PathBuf::from(&b.metadata));
                    if let Some(overrides) = &b.config {
                        for (key, value) in overrides {
                            board
                                .overrides
                                .insert(key.clone(), crate::zorro::toml_value_to_string(value));
                        }
                    }
                    board
                })
                .collect(),
            paths: raw.paths.clone(),
        })
    }

    /// Load a configuration file into the typed model, validating it.
    pub fn load_from(path: &Path) -> Result<Self> {
        let setup = Self::from_raw(&crate::config::raw_from_path(path)?)?;
        // The loaded configuration is now the one in hand, so its `[paths]`
        // is where things go from here -- a screenshot taken after loading
        // it should not still be following the one before.
        setup.apply_paths();
        Ok(setup)
    }

    /// Re-read the host MIDI endpoints for the device pickers.
    #[cfg(feature = "midi")]
    pub fn refresh_midi_endpoints(&mut self) {
        self.midi_endpoints = crate::midi::enumerate();
    }

    /// Re-read the host audio output devices for the "Audio output" picker.
    pub fn refresh_audio_devices(&mut self) {
        self.audio_devices = crate::audio::picker_output_devices();
    }

    /// Re-read the host audio input devices for the sampler "Audio input" picker.
    pub fn refresh_sampler_inputs(&mut self) {
        self.sampler_input_devices = crate::sampler::picker_input_devices();
    }

    /// The selected serial mode and parallel device, so the panel can pick the
    /// dynamic Serial/Parallel row sets (see [`rows`]).
    /// Whether the MIDI output is pointed at the built-in MT-32, which is
    /// what puts its ROM and panel rows on the I/O Ports tab.
    pub fn midi_out_is_mt32(&self) -> bool {
        crate::config::midi_out_is_mt32(self.midi_out.as_deref())
    }

    pub fn midi_out_is_csynth(&self) -> bool {
        crate::config::midi_out_is_csynth(self.midi_out.as_deref())
    }

    pub fn serial_mode(&self) -> SerialMode {
        self.serial_mode
    }

    pub fn parallel_device(&self) -> ParallelDevice {
        self.parallel_device
    }

    /// The address one of the serial TCP boxes holds, or `None` while it is
    /// unset (the run then dials nothing and binds the default).
    #[cfg(feature = "midi")]
    pub fn serial_addr(&self, field: LauncherField) -> Option<&str> {
        match field {
            F::SerialConnect => self.serial_connect.as_deref(),
            F::SerialListen => self.serial_listen.as_deref(),
            _ => None,
        }
    }

    /// Store what was typed into one of the serial TCP boxes. `None` clears
    /// the key from a saved config rather than writing an empty address.
    #[cfg(feature = "midi")]
    pub fn set_serial_addr(&mut self, field: LauncherField, addr: Option<String>) {
        match field {
            F::SerialConnect => self.serial_connect = addr,
            F::SerialListen => self.serial_listen = addr,
            _ => {}
        }
    }

    /// Whether the selected Ethernet backend carries traffic on the host's
    /// schedule rather than the emulated clock, breaking byte-identical
    /// replay (the I/O Ports tab shows a warning). The loopback backend
    /// echoes frames deterministically and an isolated or absent NIC never
    /// sees traffic, so NAT and a direct adapter bridge qualify -- as does
    /// the HostSocket board's own `Host` mode, real host OS sockets on the
    /// host's schedule same as NAT/bridge, just without an intervening
    /// `NetConfig` backend to match on.
    pub fn ethernet_breaks_determinism(&self) -> bool {
        self.hostsocket_host_mode
            || [&self.a2065_net, &self.hostsocket_net].iter().any(|net| {
                matches!(
                    net.as_ref(),
                    Some(NetConfig::Nat) if crate::net::NAT_AVAILABLE
                ) || matches!(
                    net.as_ref(),
                    Some(NetConfig::Bridge { .. }) if crate::net::BRIDGE_AVAILABLE
                )
            })
    }

    /// Re-read every host device list (MIDI endpoints + audio outputs + sampler
    /// inputs) for the pickers. Call after (re)building the setup -- e.g. loading
    /// a config or resetting to defaults -- so the pickers show what is connected
    /// now instead of an empty list that can only land on "Default"/"None".
    pub fn refresh_host_devices(&mut self) {
        #[cfg(feature = "midi")]
        self.refresh_midi_endpoints();
        self.refresh_audio_devices();
        self.refresh_sampler_inputs();
        self.refresh_bridge_interfaces();
    }

    fn refresh_bridge_interfaces(&mut self) {
        self.bridge_interfaces.clear();
        #[cfg(all(feature = "net-bridge", not(target_arch = "wasm32")))]
        match crate::net::bridge::list_interfaces() {
            Ok(interfaces) => {
                self.bridge_interfaces = interfaces
                    .into_iter()
                    .filter(|interface| !interface.loopback)
                    .map(|interface| (interface.name.clone(), interface.label()))
                    .collect();
            }
            Err(error) => log::warn!("launcher: cannot enumerate bridge adapters: {error:#}"),
        }
    }

    /// The bare-profile config this setup is compared against when emitting
    /// minimal TOML: the machine the selected profile produces with no
    /// overrides, resolved through the same `TryFrom` as a real boot so the
    /// comparison matches exactly (including derived clock/cache defaults).
    fn baseline(&self) -> Config {
        let mut raw = RawConfig::default();
        raw.machine.profile = self.model.map(|m| model_name(m).to_string());
        raw.try_into().unwrap_or_else(|_| {
            self.model
                .map_or_else(Config::default, machine_profile_defaults)
        })
    }

    /// Convert back to a raw config, emitting only the fields that differ from
    /// the selected profile's defaults (so saved files stay minimal).
    pub fn to_raw(&self) -> RawConfig {
        let base = self.baseline();
        let mut raw = RawConfig::default();
        if let Some(m) = self.model {
            raw.machine.profile = Some(model_name(m).to_string());
        }
        // System
        if self.chipset != base.chipset {
            raw.chipset.revision = Some(chipset_name(self.chipset).to_string());
        }
        if let Some(a) = self.agnus {
            raw.chipset.agnus = Some(agnus_name(a).to_string());
        }
        if let Some(d) = self.denise {
            raw.chipset.denise = Some(denise_name(d).to_string());
        }
        if self.video != base.video_standard {
            raw.chipset.video = Some(video_name(self.video).to_string());
        }
        if self.rtc != base.rtc_present {
            raw.machine.rtc = Some(self.rtc);
        }
        if self.identify != base.identify_board {
            raw.identify = Some(self.identify);
        }
        if self.rtg != base.rtg {
            raw.rtg.card = Some(rtg_card_value(self.rtg).to_string());
        }
        if self.rtg_vram_bytes != base.rtg_vram_bytes {
            raw.rtg.vram = Some(format_size(self.rtg_vram_bytes));
        }
        // CPU
        if self.cpu != base.cpu {
            raw.cpu.model = Some(cpu_name(self.cpu).to_string());
        }
        if self.fpu != base.fpu {
            raw.cpu.fpu = Some(self.fpu);
        }
        if (self.clock_mhz - base.cpu_clock_mhz).abs() > 1e-9 {
            raw.cpu.clock_mhz = Some(self.clock_mhz);
        }
        if self.icache != base.cpu_icache {
            raw.cpu.icache = Some(self.icache);
        }
        if self.dcache != base.cpu_dcache {
            raw.cpu.dcache = Some(self.dcache);
        }
        if self.jit != base.cpu_jit {
            raw.cpu.jit = Some(self.jit);
        }
        // Memory
        if self.chip_ram != base.chip_ram_bytes {
            raw.memory.chip = Some(format_size(self.chip_ram));
        }
        if self.fast_ram != base.fast_ram_bytes {
            raw.memory.fast = Some(format_size(self.fast_ram));
        }
        if self.slow_ram != base.slow_ram_bytes {
            raw.memory.slow = Some(format_size(self.slow_ram));
        }
        if self.ram_init != base.ram_init {
            raw.memory.init = Some(self.ram_init.config_value());
        }
        if self.mb_ram != base.mb_ram_bytes {
            raw.memory.motherboard = Some(format_size(self.mb_ram));
        }
        if self.accel_ram != base.accel_ram_bytes {
            raw.memory.accelerator = Some(format_size(self.accel_ram));
        }
        if self.z3_ram != base.z3_ram_bytes {
            raw.memory.z3 = Some(format_size(self.z3_ram));
        }
        // ROM
        raw.rom = self.rom.as_deref().map(path_string);
        raw.extended_rom = self.extended_rom.as_deref().map(path_string);
        // Floppy: cover any drive carrying media so the count never orphans it.
        let media_max = self
            .df_playlists
            .iter()
            .rposition(|p| !p.is_empty())
            .map(|i| i as u8 + 1)
            .unwrap_or(1);
        let drives = self.floppy_drives.max(media_max);
        if drives != 1 {
            raw.floppy.drives = Some(drives);
        }
        if self.floppy_speed != 100 {
            raw.floppy.speed = Some(self.floppy_speed);
        }
        raw.floppy.df0 = self.floppy_drive_raw(0);
        raw.floppy.df1 = self.floppy_drive_raw(1);
        raw.floppy.df2 = self.floppy_drive_raw(2);
        raw.floppy.df3 = self.floppy_drive_raw(3);
        // Hard disk
        raw.ide.master = drive_raw(
            self.ide_master.as_deref(),
            self.ide_master_name.as_deref(),
            self.effective_bootpri(F::IdeMasterBoot),
            self.ide_master_fs,
        );
        raw.ide.slave = drive_raw(
            self.ide_slave.as_deref(),
            self.ide_slave_name.as_deref(),
            self.effective_bootpri(F::IdeSlaveBoot),
            self.ide_slave_fs,
        );
        // Only emit `[scsi]` when a controller is fitted, so an unset board
        // leaves the section absent rather than writing dangling ROM/units.
        if let Some(controller) = self.scsi_controller {
            // Name every controller: which one a bare [scsi] means depends on
            // the machine (an A3000 defaults to its motherboard SCSI).
            raw.scsi.controller = Some(
                match controller {
                    ScsiController::A2091 => "a2091",
                    ScsiController::A4091 => "a4091",
                    ScsiController::A3000 => "a3000",
                }
                .to_string(),
            );
            // The motherboard SCSI has no boot ROM of its own.
            raw.scsi.rom = controller
                .is_zorro_board()
                .then(|| self.scsi_rom.as_deref().map(path_string))
                .flatten();
            // rom_odd is an A2091 split-EPROM option; the A4091 has one ROM.
            // It is the odd half OF rom, so without rom there is nothing for it
            // to complete and the config would not validate.
            raw.scsi.rom_odd = (controller == ScsiController::A2091 && raw.scsi.rom.is_some())
                .then(|| self.scsi_rom_odd.as_deref().map(path_string))
                .flatten();
            raw.scsi.unit0 = drive_raw(
                self.scsi_units[0].as_deref(),
                self.scsi_unit_names[0].as_deref(),
                self.effective_bootpri(F::ScsiUnit0Boot),
                self.scsi_unit_fs[0],
            );
            raw.scsi.unit1 = drive_raw(
                self.scsi_units[1].as_deref(),
                self.scsi_unit_names[1].as_deref(),
                self.effective_bootpri(F::ScsiUnit1Boot),
                self.scsi_unit_fs[1],
            );
            raw.scsi.unit2 = drive_raw(
                self.scsi_units[2].as_deref(),
                self.scsi_unit_names[2].as_deref(),
                self.effective_bootpri(F::ScsiUnit2Boot),
                self.scsi_unit_fs[2],
            );
            raw.scsi.unit3 = drive_raw(
                self.scsi_units[3].as_deref(),
                self.scsi_unit_names[3].as_deref(),
                self.effective_bootpri(F::ScsiUnit3Boot),
                self.scsi_unit_fs[3],
            );
            raw.scsi.unit4 = drive_raw(
                self.scsi_units[4].as_deref(),
                self.scsi_unit_names[4].as_deref(),
                self.effective_bootpri(F::ScsiUnit4Boot),
                self.scsi_unit_fs[4],
            );
            raw.scsi.unit5 = drive_raw(
                self.scsi_units[5].as_deref(),
                self.scsi_unit_names[5].as_deref(),
                self.effective_bootpri(F::ScsiUnit5Boot),
                self.scsi_unit_fs[5],
            );
            raw.scsi.unit6 = drive_raw(
                self.scsi_units[6].as_deref(),
                self.scsi_unit_names[6].as_deref(),
                self.effective_bootpri(F::ScsiUnit6Boot),
                self.scsi_unit_fs[6],
            );
        }
        // Only emit `[lide]` when a board is fitted, matching `[scsi]` above.
        if let Some(board) = self.lide_board {
            raw.lide.board = Some(board.name().to_string());
            raw.lide.rom = self.lide_rom.as_deref().map(path_string);
            // AT-Bus 2008 has no flash banking; a second bank there does not
            // validate, so the row is hidden and nothing is ever emitted for it.
            raw.lide.rom_bank2 = (board != LidePersonality::AtBus2008)
                .then(|| self.lide_rom_bank2.as_deref().map(path_string))
                .flatten();
            // `[lide] drives` is a positional list in the config file -- a hole
            // cannot be represented -- so this stops at the first empty slot
            // rather than filtering, trusting `clear_path`'s cascade to keep
            // the array itself always gap-free.
            const LIDE_DRIVE_BOOT_FIELDS: [LauncherField; 4] = [
                F::LideDrive0Boot,
                F::LideDrive1Boot,
                F::LideDrive2Boot,
                F::LideDrive3Boot,
            ];
            raw.lide.drives = (0..board.max_drives())
                .map_while(|i| {
                    drive_raw(
                        self.lide_drives[i].as_deref(),
                        self.lide_drive_names[i].as_deref(),
                        self.effective_bootpri(LIDE_DRIVE_BOOT_FIELDS[i]),
                        self.lide_drive_fs[i],
                    )
                })
                .collect();
        }
        // Host FS mounts: the edited slots (empty ones drop out), then any
        // hand-written extras beyond what the GUI shows.
        raw.filesys = (0..FILESYS_GUI_SLOTS)
            .filter_map(|i| {
                self.filesys_dirs[i].as_ref().map(|p| RawFilesysMount {
                    path: path_string(p),
                    volume: self.filesys_names[i]
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    bootpri: (self.filesys_bootpri[i] != -128).then_some(self.filesys_bootpri[i]),
                    // Emitted only when set, like bootpri: writable is the
                    // default, so an untouched config stays as written.
                    readonly: self.filesys_readonly[i].then_some(true),
                })
            })
            .chain(self.filesys_extra.iter().cloned())
            .collect();
        // Real host disks. Emitted in attachment order so the file reads the
        // way the page does. Access is always explicit; older entries with no
        // read_only field remain protected by the parser's safe default.
        raw.host_disk = crate::config::HostDiskAttach::all()
            .iter()
            .filter_map(|attach| self.host_disk_at(*attach))
            .map(|disk| RawHostDisk {
                device: disk.device.clone(),
                fingerprint: disk.fingerprint.clone(),
                identity_confirmed: false,
                attach: Some(disk.attach.token().to_string()),
                // Always state the access mode. Older hand-written entries with
                // no field stay safely read-only.
                read_only: Some(!disk.writable),
            })
            .collect();
        // CD
        raw.cd.image = self.cd_image.as_deref().map(path_string);
        if self.cd_insert_delay != 0.0 {
            raw.cd.insert_delay = Some(self.cd_insert_delay);
        }
        raw.cd.nvram = self.cd32_nvram.as_deref().map(path_string);
        // WHDLoad direct boot. `args` has no UI row but still round-trips.
        raw.whdload.game = self.whdload_game.as_deref().map(path_string);
        raw.whdload.kickstarts = self.whdload_kickstarts.as_deref().map(path_string);
        raw.whdload.library = self.whdload_library.as_deref().map(path_string);
        raw.whdload.args = self.whdload_args.clone();
        raw.whdload.whd_package = self.whdload_whd_package.as_deref().map(path_string);
        raw.whdload.skick_package = self.whdload_skick_package.as_deref().map(path_string);
        // Only written when it is not the default, so a saved file stays
        // the short list of what differs from the profile.
        raw.whdload.machine_type = (self.whdload_machine
            != crate::config::WhdloadMachine::default())
        .then_some(self.whdload_machine);
        raw.whdload.library_db = self.whdload_library_db.as_deref().map(path_string);
        raw.whdload.library_cache = self.whdload_library_cache.as_deref().map(path_string);
        // Only written when it is off, since on is the default and a
        // configuration file should say what differs from it.
        raw.whdload.enabled = (!self.whdload_enabled).then_some(false);
        raw.whdload.games = self.whdload_games.as_deref().map(path_string);
        // A/V and emulation
        if self.overscan != base.overscan {
            raw.display.overscan = Some(overscan_name(self.overscan).to_string());
        }
        if self.pixel_aspect != base.pixel_aspect {
            raw.display.pixel_aspect = Some(pixel_aspect_name(self.pixel_aspect).to_string());
        }
        if self.scaling != base.scaling {
            raw.display.scaling = Some(display_scaling_name(self.scaling).to_string());
        }
        if self.deinterlace != base.deinterlace {
            raw.display.deinterlace = Some(self.deinterlace);
        }
        if (self.phosphor - base.phosphor).abs() > 1e-6 {
            raw.display.phosphor = Some(self.phosphor);
        }
        if self.shader != base.shader {
            raw.display.shader = Some(shader_name(&self.shader));
        }
        if (self.shader_strength - base.shader_strength).abs() > 1e-6 {
            raw.display.shader_strength = Some(self.shader_strength);
        }
        if self.bezel != base.bezel {
            raw.display.bezel = Some(crate::config::RawBezel::Named(self.bezel.label().into()));
        }
        if self.bezel_stickers != base.bezel_stickers {
            raw.display.bezel_stickers = self.bezel_stickers.as_deref().map(path_string);
        }
        if self.perf_overlay != base.perf_overlay {
            raw.display.perf_overlay = Some(self.perf_overlay);
        }
        if self.tint != base.tint {
            raw.display.tint = Some(tint_name(self.tint).to_string());
        }
        if self.mt32_control_rom != base.serial.mt32_control_rom {
            raw.serial.mt32_control_rom = self
                .mt32_control_rom
                .as_ref()
                .map(|p| p.display().to_string());
        }
        if self.mt32_pcm_rom != base.serial.mt32_pcm_rom {
            raw.serial.mt32_pcm_rom = self.mt32_pcm_rom.as_ref().map(|p| p.display().to_string());
        }
        if self.mt32_panel != base.serial.mt32_panel {
            raw.serial.mt32_panel = Some(self.mt32_panel);
        }
        if self.mt32_lcd != base.serial.mt32_lcd {
            raw.serial.mt32_lcd = Some(self.mt32_lcd.label().to_string());
        }
        if self.csynth_soundfont != base.serial.coppersynth_soundfont {
            raw.serial.coppersynth_soundfont = self
                .csynth_soundfont
                .as_ref()
                .map(|p| p.display().to_string());
        }
        if self.csynth_mt32_mode != base.serial.coppersynth_mt32_mode {
            raw.serial.coppersynth_mt32_mode = self.csynth_mt32_mode.clone();
        }
        if self.csynth_panel != base.serial.coppersynth_panel {
            raw.serial.coppersynth_panel = Some(self.csynth_panel);
        }
        if self.menu_scale != base.menu_scale {
            raw.display.menu_scale = Some(self.menu_scale.label().to_string());
        }
        if self.start_fullscreen != base.full_screen {
            raw.display.full_screen = Some(self.start_fullscreen);
        }
        if self.show_status_bar != base.status_bar {
            raw.display.status_bar = Some(self.show_status_bar);
        }
        if self.floppy_sounds != base.audio.floppy_sounds {
            raw.audio.floppy_sounds = Some(self.floppy_sounds);
        }
        if self.floppy_volume != base.audio.floppy_sounds_volume {
            raw.audio.floppy_sounds_volume = Some(u16::from(self.floppy_volume));
        }
        if self.power_on != base.emulation.power_on {
            raw.emulation.power_on = Some(self.power_on);
        }
        if self.pacing_budget != base.emulation.pacing_budget {
            raw.emulation.pacing_budget = Some(pacing_name(self.pacing_budget).to_string());
        }
        if self.realtime_priority != base.emulation.realtime_priority {
            raw.emulation.realtime_priority = Some(self.realtime_priority);
        }
        if self.warp != base.emulation.warp_speed {
            raw.emulation.warp_speed = Some(self.warp.label().to_ascii_lowercase());
        }
        if self.joystick_input_mode != base.joystick_input_mode {
            raw.input.joystick = Some(self.joystick_input_mode.label().to_string());
        }
        if self.mouse_sensitivity != base.mouse_sensitivity {
            raw.input.mouse_sensitivity = Some(u16::from(self.mouse_sensitivity));
        }
        if self.mouse_capture != base.mouse_capture {
            raw.input.mouse_capture = Some(self.mouse_capture.label().to_string());
        }
        // Per port against the profile baseline, so a CD32 keeps its pad
        // implicit and a stock machine emits no port keys at all.
        if self.port_devices[0] != base.port_devices[0] {
            raw.input.port1 = Some(self.port_devices[0].label().to_string());
        }
        if self.port_devices[1] != base.port_devices[1] {
            raw.input.port2 = Some(self.port_devices[1].label().to_string());
        }
        if self.serial_mode != base.serial.mode {
            raw.serial.mode = Some(self.serial_mode.label().to_string());
        }
        raw.serial.midi_out = self.midi_out.clone();
        raw.serial.midi_in = self.midi_in.clone();
        raw.serial.listen = self.serial_listen.clone();
        raw.serial.connect = self.serial_connect.clone();
        // Parallel port. Carry each peripheral's settings whenever they are set
        // so a Save round-trips them even while another device is temporarily
        // selected. The sampler options do not imply the sampler, so they are
        // always safe to emit; a bare `output` path implies the printer, so an
        // explicit `device` disambiguates when it is carried under None.
        raw.parallel.output = self.parallel_output.as_deref().map(path_string);
        raw.parallel.sampler_input = self.sampler_input.clone();
        raw.parallel.sampler_gain = (self.sampler_gain_db != 0.0).then_some(self.sampler_gain_db);
        raw.parallel.device = match self.parallel_device {
            // None is the resolved default (omitted to keep the TOML minimal),
            // but emit it explicitly to override a carried-over `output` path
            // that would otherwise be read back as the printer.
            ParallelDevice::None => self
                .parallel_output
                .is_some()
                .then(|| ParallelDevice::None.label().to_string()),
            // A printer needs a capture file. Without one it is an incomplete
            // selection, so persist nothing (a bare `output` would already imply
            // the printer, so no explicit device is needed when it is set).
            ParallelDevice::Printer => self
                .parallel_output
                .is_some()
                .then(|| ParallelDevice::Printer.label().to_string()),
            ParallelDevice::Sampler => Some(ParallelDevice::Sampler.label().to_string()),
        };
        // Ethernet: no profile fits an A2065 by default, so the board is
        // emitted whenever it is on (absent key = not fitted).
        raw.a2065.net = self
            .a2065_net
            .as_ref()
            .map(|n| crate::net::net_config_name(n).to_string());
        raw.a2065.interface = match self.a2065_net.as_ref() {
            Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
            _ => None,
        };
        // HostSocket: same shape as the A2065 (absent key = not fitted),
        // plus the pass-through keys this screen does not edit. `net =
        // "host"` is a separate transport, not a `NetConfig` backend (see
        // `hostsocket_host_mode`'s own comment) -- when set it overrides
        // the backend name entirely and clears `interface`/`address`/
        // `gateway`, none of which apply to it (`Config::from_raw` rejects
        // any of the three alongside `net = "host"`).
        if self.hostsocket_host_mode {
            raw.hostsocket.net = Some("host".to_string());
            raw.hostsocket.interface = None;
            raw.hostsocket.address = None;
            raw.hostsocket.gateway = None;
        } else {
            raw.hostsocket.net = self
                .hostsocket_net
                .as_ref()
                .map(|n| crate::net::net_config_name(n).to_string());
            raw.hostsocket.interface = match self.hostsocket_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
                _ => None,
            };
            raw.hostsocket.address = self.hostsocket_address.clone();
            raw.hostsocket.gateway = self.hostsocket_gateway.clone();
        }
        raw.hostsocket.dns_server = self.hostsocket_dns_server.clone();
        raw.hostsocket.hostname = self.hostsocket_hostname.clone();
        raw.hostsocket.resolver = self.hostsocket_resolver.clone();
        // Sound: both boards are absent by default, so only "on" is emitted.
        if self.toccata != base.toccata {
            raw.toccata.enabled = Some(self.toccata);
        }
        if self.mhi != base.mhi {
            raw.mhi.enabled = Some(self.mhi);
        }
        // The Audio output picker is one of default / a named device / Disabled.
        // A named device sets output_device; Disabled sets output_enabled=false
        // (the resolved default is true, so it is omitted otherwise).
        raw.audio.output_device = self.audio_output.device().map(str::to_string);
        raw.audio.output_enabled = (!self.audio_output.is_enabled()).then_some(false);
        // Emit only the non-default mode; Stereo is the resolved default, so
        // omitting it keeps a default machine's TOML minimal.
        raw.audio.channel_mode = (self.audio_channel_mode != ChannelMode::Stereo)
            .then(|| self.audio_channel_mode.label().to_string());
        raw.audio.stereo_separation = (self.audio_stereo_separation != 100)
            .then_some(u16::from(self.audio_stereo_separation));
        raw.audio.audio_filter = (self.audio_filter != AudioFilterMode::Auto)
            .then(|| self.audio_filter.label().to_string());
        raw.audio.stem_granularity = self.audio_stem_granularity.as_ref().map(|list| {
            list.iter()
                .map(|g| g.as_str())
                .collect::<Vec<_>>()
                .join(",")
        });
        // Zorro boards: emit the metadata path plus any per-board overrides
        // (typed per the option schema), only when the user changed something.
        raw.zorro = self
            .zorro_boards
            .iter()
            .map(|b| {
                let mut table = toml::Table::new();
                for o in &b.options {
                    if let Some(v) = b.override_toml(o) {
                        table.insert(o.key.clone(), v);
                    }
                }
                RawZorroBoard {
                    metadata: path_string(&b.metadata),
                    config: (!table.is_empty()).then_some(table),
                }
            })
            .collect();
        // Written out whole rather than compared against the baseline: every
        // entry is already optional and skipped when unset, so an untouched
        // Paths page emits no `[paths]` at all.
        raw.paths = self.paths.clone();
        raw
    }

    fn floppy_drive_raw(&self, idx: usize) -> Option<RawFloppyDrive> {
        // A bay using a real drive writes its interface and settings instead
        // of an image; only the settings that differ from the cautious
        // defaults are emitted, so a saved config stays readable.
        if let Some(bridge) = self.df_bridge[idx]
            .as_ref()
            .filter(|_| !self.df_bridge_none[idx])
        {
            let default = FluxBridgeConfig::default();
            return Some(RawFloppyDrive {
                bridge: Some(bridge_driver_name(bridge.driver).to_string()),
                bridge_port: bridge.port.clone(),
                bridge_cable: (bridge.cable != default.cable)
                    .then(|| bridge_cable_name(bridge.cable).to_string()),
                bridge_density: (bridge.density != default.density)
                    .then(|| bridge_density_name(bridge.density).to_string()),
                bridge_mode: (bridge.mode != default.mode)
                    .then(|| bridge_mode_name(bridge.mode).to_string()),
                bridge_speed: (bridge.speed != crate::config::DEFAULT_BRIDGE_SPEED_PERCENT).then(
                    || {
                        crate::config::RawReplaySpeed::Word(
                            if bridge.speed == 200 {
                                "fast"
                            } else {
                                "normal"
                            }
                            .into(),
                        )
                    },
                ),
                // Same rule, and the same tick box, as an image: only an
                // unprotected drive says so.
                write_protected: (!self.df_write_protected[idx]).then_some(false),
                ..RawFloppyDrive::default()
            });
        }
        let playlist = &self.df_playlists[idx];
        if playlist.is_empty() {
            // A write-protect flag on an empty drive is meaningless, so an
            // untouched/empty drive emits no [floppy.dfN] table at all.
            return None;
        }
        let (first, rest) = playlist.split_first().expect("non-empty checked above");
        Some(RawFloppyDrive {
            enabled: None,
            path: Some(path_string(first)),
            paths: (!rest.is_empty()).then(|| rest.iter().map(|p| path_string(p)).collect()),
            // write_protected defaults to true; only an unprotected drive is
            // written explicitly.
            write_protected: (!self.df_write_protected[idx]).then_some(false),
            // Bridges are emitted by the FluxBridge page, not the image rows.
            ..RawFloppyDrive::default()
        })
    }

    /// Serialize the configured machine to TOML for the Save action.
    pub fn to_toml(&self) -> Result<String> {
        self.to_raw().to_toml_string()
    }

    /// Validate the configured machine, producing the [`Config`] the Run action
    /// builds from (its boot ROM may still be the AROS sentinel; the caller
    /// resolves that).
    pub fn build_config(&self) -> Result<Config> {
        self.to_raw().try_into()
    }

    pub fn model(&self) -> Option<MachineModel> {
        self.model
    }

    /// The model to show as selected in the picker. With no profile chosen the
    /// machine equals the A500 defaults, so the A500 button is highlighted.
    pub fn selected_model(&self) -> MachineModel {
        self.model.unwrap_or(MachineModel::A500)
    }

    /// Switch machine profile, resetting the profile-derived fields to the new
    /// model's defaults and dropping media the new model cannot use (so a
    /// later Run does not fail validation on a stale IDE/CD image). Boot media
    /// the model can still carry (ROM, floppies, SCSI, Zorro) is kept.
    pub fn select_model(&mut self, model: Option<MachineModel>) {
        self.model = model;
        let base = self.baseline();
        self.chipset = base.chipset;
        self.agnus = None;
        self.denise = None;
        self.video = base.video_standard;
        self.rtc = base.rtc_present;
        self.identify = base.identify_board;
        self.rtg = base.rtg;
        self.rtg_vram_bytes = base.rtg_vram_bytes;
        self.cpu = base.cpu;
        self.fpu = base.fpu;
        self.clock_mhz = base.cpu_clock_mhz;
        self.icache = base.cpu_icache;
        self.dcache = base.cpu_dcache;
        self.jit = base.cpu_jit;
        self.chip_ram = base.chip_ram_bytes;
        self.fast_ram = base.fast_ram_bytes;
        self.slow_ram = base.slow_ram_bytes;
        self.mb_ram = base.mb_ram_bytes;
        self.accel_ram = base.accel_ram_bytes;
        self.z3_ram = base.z3_ram_bytes;
        self.overscan = base.overscan;
        self.pixel_aspect = base.pixel_aspect;
        self.scaling = base.scaling;
        self.deinterlace = base.deinterlace;
        self.phosphor = base.phosphor;
        // The remembered user-shader path survives: it came from the config
        // file, not from the profile, and the picker needs it to offer
        // "Custom" again.
        self.shader = base.shader.clone();
        self.shader_strength = base.shader_strength;
        self.bezel = base.bezel;
        // The sticker folder survives like the shader path above: it names
        // a folder of the user's, not anything of the profile's.
        self.perf_overlay = base.perf_overlay;
        self.tint = base.tint;
        self.menu_scale = base.menu_scale;
        self.mt32_control_rom = base.serial.mt32_control_rom.clone();
        self.mt32_pcm_rom = base.serial.mt32_pcm_rom.clone();
        self.mt32_panel = base.serial.mt32_panel;
        self.mt32_lcd = base.serial.mt32_lcd;
        self.csynth_soundfont = base.serial.coppersynth_soundfont.clone();
        self.csynth_mt32_mode = base.serial.coppersynth_mt32_mode.clone();
        self.csynth_panel = base.serial.coppersynth_panel;
        self.start_fullscreen = base.full_screen;
        self.show_status_bar = base.status_bar;
        self.floppy_sounds = base.audio.floppy_sounds;
        self.floppy_volume = base.audio.floppy_sounds_volume;
        self.power_on = base.emulation.power_on;
        self.pacing_budget = base.emulation.pacing_budget;
        self.realtime_priority = base.emulation.realtime_priority;
        self.warp = base.emulation.warp_speed;
        self.joystick_input_mode = base.joystick_input_mode;
        self.mouse_sensitivity = base.mouse_sensitivity;
        self.mouse_capture = base.mouse_capture;
        self.port_devices = base.port_devices;
        if !self.has_ide() {
            self.ide_master = None;
            self.ide_master_name = None;
            self.ide_master_bootpri = None;
            self.ide_slave = None;
            self.ide_slave_name = None;
            self.ide_slave_bootpri = None;
        }
        if !self.has_cd() {
            self.cd_image = None;
            self.cd_insert_delay = 0.0;
        }
        if model != Some(MachineModel::Cd32) {
            self.cd32_nvram = None;
        }
        // The motherboard SCSI leaves with the motherboard; the drives stay and
        // land on the default Zorro board instead.
        if !self.has_sdmac() && self.scsi_controller == Some(ScsiController::A3000) {
            self.scsi_controller = Some(ScsiController::A2091);
        }
        // Last, once the ports this machine has are settled: a real disk on
        // one it does not have goes the same way an image on it does.
        self.drop_unreachable_host_disks();
    }

    fn has_gayle(&self) -> bool {
        matches!(self.model, Some(MachineModel::A600 | MachineModel::A1200))
    }

    fn has_sdmac(&self) -> bool {
        self.model == Some(MachineModel::A3000)
    }

    /// Whether a SCSI controller is fitted, which is what a SCSI unit needs
    /// to exist at all. The A3000's choice needs the motherboard silicon
    /// behind it; a Zorro board needs only to have been chosen.
    pub fn has_scsi_controller(&self) -> bool {
        match self.scsi_controller {
            Some(ScsiController::A3000) => self.has_sdmac(),
            Some(_) => true,
            None => false,
        }
    }

    /// Machines with an IDE port: Gayle's, and the A4000's at $DD2020.
    pub fn has_ide(&self) -> bool {
        self.has_gayle() || self.model == Some(MachineModel::A4000)
    }

    fn has_cd(&self) -> bool {
        matches!(self.model, Some(MachineModel::Cdtv | MachineModel::Cd32))
    }

    /// Whether a field is applicable to the current machine (greyed otherwise).
    pub fn applies(&self, field: LauncherField) -> bool {
        self.disabled_reason(field).is_none()
    }

    /// Whether a row is hidden entirely (not just greyed). The Floppy tab only
    /// shows a drive's rows once it is wired in, so unused drives simply do not
    /// appear rather than sitting greyed -- there is never a reason to touch a
    /// drive that is not enabled.
    pub fn row_hidden(&self, field: LauncherField) -> bool {
        use LauncherField as F;
        match field {
            F::Df1Image | F::Df1WriteProtect => self.floppy_drives < 2,
            F::Df2Image | F::Df2WriteProtect => self.floppy_drives < 3,
            F::Df3Image | F::Df3WriteProtect => self.floppy_drives < 4,
            // The word only exists while the fill is Fixed; the other
            // fills have nothing to edit, so the row goes rather than
            // sitting greyed under a setting most people never touch.
            F::RamPattern => !matches!(self.ram_init, RamInit::Pattern { .. }),
            F::EthernetInterface => {
                !matches!(self.a2065_net.as_ref(), Some(NetConfig::Bridge { .. }))
            }
            F::HostSocketInterface => {
                !matches!(self.hostsocket_net.as_ref(), Some(NetConfig::Bridge { .. }))
            }
            // A controller is not a disk: only the units carrying one have a
            // place in the boot order, so the empty six or seven go rather
            // than standing in a column saying so.
            F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot => {
                self.scsi_controller.is_none()
                    || Self::boot_field_drive(field)
                        .and_then(|drive| self.drive_holds(drive))
                        .is_none()
            }
            F::LideDrive0Boot | F::LideDrive1Boot | F::LideDrive2Boot | F::LideDrive3Boot => {
                self.lide_board.is_none()
                    || Self::boot_field_drive(field)
                        .and_then(|drive| self.drive_holds(drive))
                        .is_none()
            }
            // Nothing to configure without a board fitted.
            F::LideRom => self.lide_board.is_none(),
            // AT-Bus 2008 has no flash banking.
            F::LideRomBank2 => self
                .lide_board
                .is_none_or(|b| b == LidePersonality::AtBus2008),
            // `[lide] drives` is a positional list in the config file -- a
            // hole cannot be represented -- so a slot beyond the board's
            // channel count (RIDE/AT-Bus 2008 have one; RIPPLE has two) or
            // beyond the first empty slot stays hidden: it is not just
            // inapplicable, filling it would be unrepresentable.
            F::LideDrive0 | F::LideDrive1 | F::LideDrive2 | F::LideDrive3 => {
                lide_drive_index(field).is_some_and(|i| {
                    self.lide_board.is_none_or(|b| b.max_drives() <= i)
                        || (i > 0 && self.lide_drives[i - 1].is_none())
                })
            }
            _ => false,
        }
    }

    /// Why a field is greyed out for the current machine, shown in place of its
    /// controls so the constraint is explained rather than just disabled.
    /// `None` means the field is editable.
    pub fn disabled_reason(&self, field: LauncherField) -> Option<&'static str> {
        // `reason` is returned when the applicability condition is *false*.
        let reason = |applicable: bool, why: &'static str| (!applicable).then_some(why);
        match field {
            F::Fpu => reason(self.cpu != CpuModel::M68000, "needs 68020+"),
            // Gate on the model's actual cache capability so the launcher tracks
            // CpuModel rather than a second hand-maintained list (the 040 has
            // both caches; only the 68000 has neither).
            F::Icache => reason(self.cpu.has_instruction_cache(), "needs 68020+"),
            F::Dcache => reason(self.cpu.has_data_cache(), "needs 68030/040"),
            // The 68000/68010 shared-bus float model needs the precise
            // core, so the JIT never engages there (see cpu.rs).
            F::Jit => reason(
                !matches!(self.cpu, CpuModel::M68000 | CpuModel::M68010),
                "needs 68020+",
            ),

            F::Z3Ram => reason(cpu_is_32bit(self.cpu), "needs 32-bit CPU"),
            // The CPU-slot space at $08000000 is beyond a 24-bit bus too.
            F::AccelRam => reason(cpu_is_32bit(self.cpu), "needs 32-bit CPU"),
            // The Zorro II cards (Picasso II/II+, Graffity [Zorro II]) remain
            // available on a 24-bit CPU. The stepper's choice list omits the
            // Zorro III-only cards (Graffity [Zorro III], Z3660) in that case.
            F::Rtg => None,
            // Motherboard fast RAM hangs off Ramsey, which only the big-box
            // profiles fit, and its bank ends beyond a 24-bit address bus.
            F::MbRam => {
                let big_box = matches!(self.model, Some(MachineModel::A3000 | MachineModel::A4000));
                reason(
                    big_box && cpu_is_32bit(self.cpu),
                    if big_box {
                        "needs 32-bit CPU"
                    } else {
                        "needs A3000/A4000"
                    },
                )
            }
            F::IdeMaster | F::IdeSlave => {
                reason(self.has_ide(), "needs A600/A1200/A4000 (or Lide)")
            }
            // The ROM and drives belong to the fitted controller; greyed with
            // none. The A3000's motherboard SCSI has no ROM of its own, and
            // rom_odd is an A2091 split-EPROM option only.
            F::ScsiRom => reason(
                self.scsi_controller
                    .is_some_and(ScsiController::is_zorro_board),
                if self.scsi_controller.is_some() {
                    "Zorro boards only"
                } else {
                    "no controller"
                },
            ),
            F::ScsiUnit0
            | F::ScsiUnit1
            | F::ScsiUnit2
            | F::ScsiUnit3
            | F::ScsiUnit4
            | F::ScsiUnit5
            | F::ScsiUnit6 => reason(self.scsi_controller.is_some(), "no controller"),
            F::ScsiRomOdd => reason(
                self.scsi_controller == Some(ScsiController::A2091),
                "A2091 only",
            ),
            F::CdImage | F::CdInsertDelay => {
                reason(self.has_cd(), "needs CDTV/CD32 (or SCSI/IDE/Lide)")
            }
            F::Cd32Nvram => reason(self.model == Some(MachineModel::Cd32), "CD32 only"),
            F::Df0Image | F::Df0WriteProtect => reason(self.floppy_drives >= 1, "drive off"),
            F::Df1Image | F::Df1WriteProtect => reason(self.floppy_drives >= 2, "drive off"),
            F::Df2Image | F::Df2WriteProtect => reason(self.floppy_drives >= 3, "drive off"),
            F::Df3Image | F::Df3WriteProtect => reason(self.floppy_drives >= 4, "drive off"),
            // Drive speed shapes how fast a track is served from an image; a
            // real drive's data rate is the disk's own. With every fitted bay
            // physical there is nothing for it to act on.
            F::FloppySpeed => {
                let any_image = self.floppy_drives == 0
                    || (0..self.floppy_drives as usize).any(|i| self.df_bridge[i].is_none());
                reason(any_image, "no image drives")
            }
            // Shader strength only feeds the shader pass, which does not run when
            // the shader is off.
            F::ShaderStrength => reason(self.shader != ShaderMode::None, "Disabled"),
            // A boot priority or read-only flag is meaningless without a
            // directory to mount.
            F::Filesys0Boot | F::Filesys1Boot | F::Filesys2Boot | F::Filesys3Boot => {
                let (slot, _) = filesys_slot(field).expect("boot field");
                reason(self.filesys_dirs[slot].is_some(), "no directory")
            }
            // Boot priority applies only to a hard-disk image: greyed for an
            // empty slot, or a CD image (which boots by its own rules).
            F::IdeMasterBoot
            | F::IdeSlaveBoot
            | F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot
            | F::LideDrive0Boot
            | F::LideDrive1Boot
            | F::LideDrive2Boot
            | F::LideDrive3Boot => {
                let drive = Self::boot_field_drive(field).expect("boot field");
                match self.drive_holds(drive) {
                    None => Some("No drive"),
                    Some(DriveContents::Image(p)) if crate::config::is_cd_image_path(p) => {
                        Some("CD-ROM")
                    }
                    // A real disk's partitions carry their own de_BootPri, on
                    // the disk, and nothing here overrides that. Naming what
                    // the slot holds says as much and reads plainer.
                    Some(DriveContents::HostDisk) => Some("Host Disk"),
                    Some(DriveContents::Image(_)) => None,
                }
            }
            F::Filesys0ReadOnly
            | F::Filesys1ReadOnly
            | F::Filesys2ReadOnly
            | F::Filesys3ReadOnly => {
                let slot = filesys_readonly_slot(field).expect("readonly field");
                reason(self.filesys_dirs[slot].is_some(), "no directory")
            }
            // The MIDI endpoint and sampler input/gain rows are hidden entirely
            // when inactive (see `rows`), so they never need a greyed state.
            // Channel mode and separation shape the output, so they do nothing
            // once audio is disabled; separation also does nothing in mono.
            // The bridge page follows what is there. A loaded config can
            // pull the bay out from under the page: every row greys, the
            // Interface one included. With the bay bridged but no interface
            // attached or selected, only the Interface row stays live -- the
            // rest describe hardware that is not present.
            #[cfg(feature = "fluxbridge")]
            F::BridgeDevice => reason(self.bridge_edit().is_some(), "No drive"),
            #[cfg(feature = "fluxbridge")]
            F::BridgeCable | F::BridgeDensity | F::BridgeReadMode | F::BridgeReplaySpeed
                if self.bridge_edit().is_none()
                    || self.df_bridge_none[self.bridge_edit_drive]
                    || self.bridge_status == BridgeStatus::NoInterface =>
            {
                Some("no interface")
            }
            // The port row stays live while there is a port to pick, even
            // with nothing recognised as attached: an interface on a serial
            // chip the scan does not name is selected by hand. It greys with
            // the interface set to None, and with nothing to pick (a list of
            // just "Automatic").
            #[cfg(feature = "fluxbridge")]
            F::BridgePort => {
                if self.bridge_edit().is_none() || self.df_bridge_none[self.bridge_edit_drive] {
                    Some("no interface")
                } else if self.bridge_ports.len() <= 1 {
                    Some("no ports")
                } else {
                    reason(
                        self.bridge_driver_supports(crate::fluxbridge::config_option::COM_PORT),
                        "not on this interface",
                    )
                }
            }
            #[cfg(feature = "fluxbridge")]
            F::BridgeCable => reason(
                self.bridge_driver_supports(crate::fluxbridge::config_option::DRIVE_AB_CABLE)
                    || self
                        .bridge_driver_supports(crate::fluxbridge::config_option::SUPPORTS_SHUGART),
                "not on this interface",
            ),
            F::AudioChannelMode => reason(self.audio_output.is_enabled(), "off"),
            F::AudioFilter => reason(self.audio_output.is_enabled(), "off"),
            F::AudioStereoSeparation => {
                if !self.audio_output.is_enabled() {
                    Some("off")
                } else {
                    reason(self.audio_channel_mode != ChannelMode::Mono, "mono")
                }
            }
            // Neither mouse row does anything unless a port holds a mouse.
            F::MouseSensitivity | F::MouseCapture => {
                reason(self.port_devices.contains(&PortDevice::Mouse), "No mouse")
            }
            _ => None,
        }
    }

    /// The current boolean of a toggle field.
    /// Whether WHDLoad has a link of its own in the left-hand strip.
    pub fn whdload_enabled(&self) -> bool {
        self.whdload_enabled
    }

    pub fn toggle_value(&self, field: LauncherField) -> bool {
        match field {
            F::Rtc => self.rtc,
            F::Identify => self.identify,
            F::Fpu => self.fpu,
            F::Icache => self.icache,
            F::Dcache => self.dcache,
            F::Jit => self.jit,
            F::Df0WriteProtect => self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3],
            F::FloppySounds => self.floppy_sounds,
            F::StartFullscreen => self.start_fullscreen,
            F::ShowStatusBar => self.show_status_bar,
            F::Deinterlace => self.deinterlace,
            F::PerfOverlay => self.perf_overlay,
            F::Mt32Panel => self.mt32_panel,
            F::PowerOn => self.power_on,
            F::RealtimePriority => self.realtime_priority,
            F::Toccata => self.toccata,
            #[cfg(feature = "mhi")]
            F::Mhi => self.mhi,
            _ => false,
        }
    }

    /// Whether the SCSI controller is the A4091, whose boot ROM has a
    /// bundled default the row reads as one.
    pub fn scsi_controller_is_a4091(&self) -> bool {
        matches!(self.scsi_controller, Some(ScsiController::A4091))
    }

    /// The current path of a path field, if any.
    pub fn path(&self, field: LauncherField) -> Option<&Path> {
        match field {
            F::Rom => self.rom.as_deref(),
            F::Mt32ControlRom => self.mt32_control_rom.as_deref(),
            #[cfg(feature = "coppersynth")]
            F::CsynthSoundfont => self.csynth_soundfont.as_deref(),
            F::Mt32PcmRom => self.mt32_pcm_rom.as_deref(),
            F::ExtendedRom => self.extended_rom.as_deref(),
            F::Df0Image => self.df_playlists[0].first().map(PathBuf::as_path),
            F::Df1Image => self.df_playlists[1].first().map(PathBuf::as_path),
            F::Df2Image => self.df_playlists[2].first().map(PathBuf::as_path),
            F::Df3Image => self.df_playlists[3].first().map(PathBuf::as_path),
            F::IdeMaster => self.ide_master.as_deref(),
            F::IdeSlave => self.ide_slave.as_deref(),
            F::ScsiRom => self.scsi_rom.as_deref(),
            F::ScsiRomOdd => self.scsi_rom_odd.as_deref(),
            F::ScsiUnit0 => self.scsi_units[0].as_deref(),
            F::ScsiUnit1 => self.scsi_units[1].as_deref(),
            F::ScsiUnit2 => self.scsi_units[2].as_deref(),
            F::ScsiUnit3 => self.scsi_units[3].as_deref(),
            F::ScsiUnit4 => self.scsi_units[4].as_deref(),
            F::ScsiUnit5 => self.scsi_units[5].as_deref(),
            F::ScsiUnit6 => self.scsi_units[6].as_deref(),
            F::LideRom => self.lide_rom.as_deref(),
            F::LideRomBank2 => self.lide_rom_bank2.as_deref(),
            F::LideDrive0 => self.lide_drives[0].as_deref(),
            F::LideDrive1 => self.lide_drives[1].as_deref(),
            F::LideDrive2 => self.lide_drives[2].as_deref(),
            F::LideDrive3 => self.lide_drives[3].as_deref(),
            F::Filesys0Dir => self.filesys_dirs[0].as_deref(),
            F::Filesys1Dir => self.filesys_dirs[1].as_deref(),
            F::Filesys2Dir => self.filesys_dirs[2].as_deref(),
            F::Filesys3Dir => self.filesys_dirs[3].as_deref(),
            F::CdImage => self.cd_image.as_deref(),
            F::Cd32Nvram => self.cd32_nvram.as_deref(),
            F::ParallelOutput => self.parallel_output.as_deref(),
            F::WhdloadGame => self.whdload_game.as_deref(),
            F::WhdloadKickstarts => self.whdload_kickstarts.as_deref(),
            F::WhdloadLibrary => self.whdload_library.as_deref(),
            F::WhdloadWhdPackage => self.whdload_whd_package.as_deref(),
            F::WhdloadSkickPackage => self.whdload_skick_package.as_deref(),
            F::WhdloadGames => self.whdload_games.as_deref(),
            // What the entry says, not where it resolves to: this is the
            // value being edited. `paths_resolved` answers the other
            // question, and the row shows that one.
            _ => self.paths_entry_ref(field).and_then(Option::as_deref),
        }
    }

    /// The `[paths]` entry a Paths row edits.
    ///
    /// `None` for anything that is not a Paths row, which is what keeps the
    /// twelve directories out of every other path accessor's way.
    fn paths_entry(
        paths: &mut crate::pathconf::Paths,
        field: LauncherField,
    ) -> Option<&mut Option<PathBuf>> {
        let p = paths;
        Some(match field {
            F::PathsBase => &mut p.base,
            F::PathsStates => &mut p.states,
            F::PathsScreenshots => &mut p.screenshots,
            F::PathsRecordings => &mut p.recordings,
            F::PathsNvram => &mut p.nvram,
            F::PathsTraces => &mut p.traces,
            F::PathsConfigs => &mut p.configs,
            F::PathsRoms => &mut p.roms,
            F::PathsFloppies => &mut p.floppies,
            F::PathsHarddrives => &mut p.harddrives,
            F::PathsCds => &mut p.cds,
            _ => return None,
        })
    }

    /// The same entry, to read. Written out twice because a borrow cannot
    /// be handed back from a copy; the test below walks every row through
    /// both, so the pair cannot drift apart unnoticed.
    fn paths_entry_ref(&self, field: LauncherField) -> Option<&Option<PathBuf>> {
        let p = &self.paths;
        Some(match field {
            F::PathsBase => &p.base,
            F::PathsStates => &p.states,
            F::PathsScreenshots => &p.screenshots,
            F::PathsRecordings => &p.recordings,
            F::PathsNvram => &p.nvram,
            F::PathsTraces => &p.traces,
            F::PathsConfigs => &p.configs,
            F::PathsRoms => &p.roms,
            F::PathsFloppies => &p.floppies,
            F::PathsHarddrives => &p.harddrives,
            F::PathsCds => &p.cds,
            _ => return None,
        })
    }

    /// Where a Paths row points now, resolved -- which for an unset row is
    /// the directory it inherits. The page is there to say where things
    /// actually go, so an inherited row still names a directory rather than
    /// going blank and leaving the answer somewhere else.
    pub fn paths_resolved(&self, field: LauncherField) -> Option<PathBuf> {
        let host = crate::paths::config_dir()?;
        let p = &self.paths;
        Some(match field {
            F::PathsBase => p.base_dir(&host),
            F::PathsStates => p.states_dir(&host),
            F::PathsScreenshots => p.screenshots_dir(&host),
            F::PathsRecordings => p.recordings_dir(&host),
            F::PathsNvram => p.nvram_dir(&host),
            F::PathsTraces => p.traces_dir(&host),
            F::PathsConfigs => p.configs_dir(&host),
            F::PathsRoms => p.roms_dir(&host),
            F::PathsFloppies => p.floppies_dir(&host),
            F::PathsHarddrives => p.harddrives_dir(&host),
            F::PathsCds => p.cds_dir(&host),
            _ => return None,
        })
    }

    /// Whether a Paths row was set rather than inherited.
    pub fn paths_is_set(&self, field: LauncherField) -> bool {
        self.paths_entry_ref(field).is_some_and(Option::is_some)
    }

    /// What a path row shows when the whole path is the point rather than
    /// the file at the end of it: the printer's capture file, and every row
    /// on the Paths page. `None` leaves the row to its usual file name.
    pub fn full_path_label(&self, field: LauncherField) -> Option<String> {
        if field == F::ParallelOutput {
            return Some(match self.path(field) {
                Some(path) => path.display().to_string(),
                None => "(none)".to_string(),
            });
        }
        if !field.is_paths_field() {
            return None;
        }
        // An inheriting row says so and stops there. Where a default goes
        // is Copperline's business until somebody makes it theirs, and a
        // page of eleven paths nobody chose is a page nobody reads.
        //
        // The base is the exception: it is the root the rest of them hang
        // off, so it names its directory whether or not it was set. With
        // it inheriting too, the page would say where nothing goes.
        if !self.paths_is_set(field) && field != F::PathsBase {
            return Some("(default)".to_string());
        }
        let resolved = self
            .paths_resolved(field)
            .or_else(|| self.path(field).map(Path::to_path_buf));
        Some(match resolved {
            Some(dir) => dir.display().to_string(),
            // No host directory at all, so nothing is resolvable and the
            // defaults land beside wherever Copperline was started.
            None => "(default)".to_string(),
        })
    }

    /// Put the edited directories in force.
    ///
    /// They are saved with the configuration like everything else on these
    /// pages -- `[paths]` is a section of it, not a file of its own -- but
    /// they take effect the moment they are changed, so a screenshot taken
    /// after moving the row lands where the row now says rather than where
    /// it said when the launcher opened.
    fn apply_paths(&self) {
        crate::paths::adopt(self.paths.clone());
    }

    /// Store a directory a Paths row was pointed at.
    ///
    /// A directory under the base is stored relative to it, so a tree that
    /// was set up as one piece still moves as one piece: point the base at
    /// a memory stick and everything under it follows, which is the whole
    /// reason the entries resolve the way they do. The base itself is
    /// always absolute -- a relative base is taken from the host-data
    /// directory, and quietly re-anchoring it there is not what somebody
    /// picking a folder meant.
    fn set_paths_dir(&mut self, field: LauncherField, dir: PathBuf) {
        let relative = (field != F::PathsBase)
            .then(|| {
                let base = crate::paths::config_dir()?;
                let base = self.paths.base_dir(&base);
                dir.strip_prefix(base).ok().map(Path::to_path_buf)
            })
            .flatten();
        if let Some(entry) = Self::paths_entry(&mut self.paths, field) {
            *entry = Some(relative.unwrap_or(dir));
        }
    }

    /// Whether `field` is a hard-drive image that can carry a volume-name
    /// override (IDE/SCSI drives, but not the SCSI boot ROM or CD/ROM paths).
    pub fn is_drive_field(field: LauncherField) -> bool {
        matches!(
            field,
            F::IdeMaster
                | F::IdeSlave
                | F::ScsiUnit0
                | F::ScsiUnit1
                | F::ScsiUnit2
                | F::ScsiUnit3
                | F::ScsiUnit4
                | F::ScsiUnit5
                | F::ScsiUnit6
                | F::LideDrive0
                | F::LideDrive1
                | F::LideDrive2
                | F::LideDrive3
                | F::Filesys0Dir
                | F::Filesys1Dir
                | F::Filesys2Dir
                | F::Filesys3Dir
        )
    }

    /// The volume-name override for a drive field, if set.
    pub fn drive_name(&self, field: LauncherField) -> Option<&str> {
        let name = match field {
            F::IdeMaster => &self.ide_master_name,
            F::IdeSlave => &self.ide_slave_name,
            F::ScsiUnit0 => &self.scsi_unit_names[0],
            F::ScsiUnit1 => &self.scsi_unit_names[1],
            F::ScsiUnit2 => &self.scsi_unit_names[2],
            F::ScsiUnit3 => &self.scsi_unit_names[3],
            F::ScsiUnit4 => &self.scsi_unit_names[4],
            F::ScsiUnit5 => &self.scsi_unit_names[5],
            F::ScsiUnit6 => &self.scsi_unit_names[6],
            F::LideDrive0 => &self.lide_drive_names[0],
            F::LideDrive1 => &self.lide_drive_names[1],
            F::LideDrive2 => &self.lide_drive_names[2],
            F::LideDrive3 => &self.lide_drive_names[3],
            F::Filesys0Dir => &self.filesys_names[0],
            F::Filesys1Dir => &self.filesys_names[1],
            F::Filesys2Dir => &self.filesys_names[2],
            F::Filesys3Dir => &self.filesys_names[3],
            _ => return None,
        };
        name.as_deref()
    }

    /// The in-memory volume's filesystem for a directory-mount drive field
    /// (FFS by default). Meaningless -- but still tracked, so a toggle made
    /// before Browse is not lost -- for a field whose path is not currently
    /// a directory; `to_raw` only emits it once the path actually is one.
    pub fn drive_filesystem(&self, field: LauncherField) -> crate::diskimage::FileSystem {
        match field {
            F::IdeMaster => self.ide_master_fs,
            F::IdeSlave => self.ide_slave_fs,
            F::ScsiUnit0 => self.scsi_unit_fs[0],
            F::ScsiUnit1 => self.scsi_unit_fs[1],
            F::ScsiUnit2 => self.scsi_unit_fs[2],
            F::ScsiUnit3 => self.scsi_unit_fs[3],
            F::ScsiUnit4 => self.scsi_unit_fs[4],
            F::ScsiUnit5 => self.scsi_unit_fs[5],
            F::ScsiUnit6 => self.scsi_unit_fs[6],
            F::LideDrive0 => self.lide_drive_fs[0],
            F::LideDrive1 => self.lide_drive_fs[1],
            F::LideDrive2 => self.lide_drive_fs[2],
            F::LideDrive3 => self.lide_drive_fs[3],
            _ => crate::diskimage::FileSystem::FFS,
        }
    }

    /// Whether a disk-backed drive field's current path is a host directory
    /// (as opposed to an image file) -- sampled when the path was last set
    /// or loaded, not by statting it here: see `ide_master_is_dir`'s doc
    /// comment. `false` for any field that is not one of the drive fields
    /// this applies to, matching `drive_filesystem`'s fallback above.
    pub fn drive_is_directory(&self, field: LauncherField) -> bool {
        match field {
            F::IdeMaster => self.ide_master_is_dir,
            F::IdeSlave => self.ide_slave_is_dir,
            F::ScsiUnit0 => self.scsi_unit_is_dir[0],
            F::ScsiUnit1 => self.scsi_unit_is_dir[1],
            F::ScsiUnit2 => self.scsi_unit_is_dir[2],
            F::ScsiUnit3 => self.scsi_unit_is_dir[3],
            F::ScsiUnit4 => self.scsi_unit_is_dir[4],
            F::ScsiUnit5 => self.scsi_unit_is_dir[5],
            F::ScsiUnit6 => self.scsi_unit_is_dir[6],
            F::LideDrive0 => self.lide_drive_is_dir[0],
            F::LideDrive1 => self.lide_drive_is_dir[1],
            F::LideDrive2 => self.lide_drive_is_dir[2],
            F::LideDrive3 => self.lide_drive_is_dir[3],
            _ => false,
        }
    }

    /// Refresh a drive field's cached directory flag from its current path
    /// (the one host-filesystem stat the field's `_is_dir` companion is
    /// allowed: on the path actually changing, not on every draw). A no-op
    /// for a field this doesn't apply to.
    fn refresh_drive_is_dir(&mut self, field: LauncherField) {
        let is_dir = self.path(field).is_some_and(|p| p.is_dir());
        let slot = match field {
            F::IdeMaster => &mut self.ide_master_is_dir,
            F::IdeSlave => &mut self.ide_slave_is_dir,
            F::ScsiUnit0 => &mut self.scsi_unit_is_dir[0],
            F::ScsiUnit1 => &mut self.scsi_unit_is_dir[1],
            F::ScsiUnit2 => &mut self.scsi_unit_is_dir[2],
            F::ScsiUnit3 => &mut self.scsi_unit_is_dir[3],
            F::ScsiUnit4 => &mut self.scsi_unit_is_dir[4],
            F::ScsiUnit5 => &mut self.scsi_unit_is_dir[5],
            F::ScsiUnit6 => &mut self.scsi_unit_is_dir[6],
            F::LideDrive0 => &mut self.lide_drive_is_dir[0],
            F::LideDrive1 => &mut self.lide_drive_is_dir[1],
            F::LideDrive2 => &mut self.lide_drive_is_dir[2],
            F::LideDrive3 => &mut self.lide_drive_is_dir[3],
            _ => return,
        };
        *slot = is_dir;
    }

    /// Set a drive field's directory-mount filesystem.
    pub fn set_drive_filesystem(&mut self, field: LauncherField, fs: crate::diskimage::FileSystem) {
        let slot = match field {
            F::IdeMaster => &mut self.ide_master_fs,
            F::IdeSlave => &mut self.ide_slave_fs,
            F::ScsiUnit0 => &mut self.scsi_unit_fs[0],
            F::ScsiUnit1 => &mut self.scsi_unit_fs[1],
            F::ScsiUnit2 => &mut self.scsi_unit_fs[2],
            F::ScsiUnit3 => &mut self.scsi_unit_fs[3],
            F::ScsiUnit4 => &mut self.scsi_unit_fs[4],
            F::ScsiUnit5 => &mut self.scsi_unit_fs[5],
            F::ScsiUnit6 => &mut self.scsi_unit_fs[6],
            F::LideDrive0 => &mut self.lide_drive_fs[0],
            F::LideDrive1 => &mut self.lide_drive_fs[1],
            F::LideDrive2 => &mut self.lide_drive_fs[2],
            F::LideDrive3 => &mut self.lide_drive_fs[3],
            _ => return,
        };
        *slot = fs;
    }

    /// Flip a drive field's directory-mount filesystem between FFS and OFS
    /// -- the Storage tab's filesystem button is a two-way toggle, not a
    /// stepper, so it needs no forward/backward direction.
    pub fn cycle_drive_filesystem(&mut self, field: LauncherField) {
        let next = if self.drive_filesystem(field).ffs {
            crate::diskimage::FileSystem::OFS
        } else {
            crate::diskimage::FileSystem::FFS
        };
        self.set_drive_filesystem(field, next);
    }

    /// Set (or, with a blank string, clear) a drive field's volume-name
    /// override. A name without a configured image is meaningless, so it is
    /// dropped when the field has no path.
    pub fn set_drive_name(&mut self, field: LauncherField, name: String) {
        let trimmed = name.trim();
        let value =
            (!trimmed.is_empty() && self.path(field).is_some()).then(|| trimmed.to_string());
        let slot = match field {
            F::IdeMaster => &mut self.ide_master_name,
            F::IdeSlave => &mut self.ide_slave_name,
            F::ScsiUnit0 => &mut self.scsi_unit_names[0],
            F::ScsiUnit1 => &mut self.scsi_unit_names[1],
            F::ScsiUnit2 => &mut self.scsi_unit_names[2],
            F::ScsiUnit3 => &mut self.scsi_unit_names[3],
            F::ScsiUnit4 => &mut self.scsi_unit_names[4],
            F::ScsiUnit5 => &mut self.scsi_unit_names[5],
            F::ScsiUnit6 => &mut self.scsi_unit_names[6],
            F::LideDrive0 => &mut self.lide_drive_names[0],
            F::LideDrive1 => &mut self.lide_drive_names[1],
            F::LideDrive2 => &mut self.lide_drive_names[2],
            F::LideDrive3 => &mut self.lide_drive_names[3],
            F::Filesys0Dir => &mut self.filesys_names[0],
            F::Filesys1Dir => &mut self.filesys_names[1],
            F::Filesys2Dir => &mut self.filesys_names[2],
            F::Filesys3Dir => &mut self.filesys_names[3],
            _ => return,
        };
        *slot = value;
    }

    /// The Input tab's live summary: which host input ends up driving
    /// each port under the chosen devices and joystick-input mode.
    /// Computed by the same routing function the runtime input pump
    /// uses, so the promise cannot drift from the behavior.
    pub fn input_routing_summary(&self) -> [String; 2] {
        let routing =
            crate::video::window::host_routing_for(self.port_devices, self.joystick_input_mode);
        std::array::from_fn(|port| {
            let source = if routing.mouse == Some(port) {
                "the host mouse".to_string()
            } else if routing.gamepad == Some(port) && routing.keyboard2 == Some(port) {
                "the gamepad (numpad keys without a pad)".to_string()
            } else if routing.gamepad == Some(port) {
                "the gamepad".to_string()
            } else if routing.keyboard == Some(port) {
                if self.port_devices[port] == PortDevice::Mouse {
                    "cursor keys as a mouse (fire keys = buttons)".to_string()
                } else {
                    "cursor keys (Ctrl/RAlt = fire, LAlt = button 2)".to_string()
                }
            } else {
                match self.port_devices[port] {
                    PortDevice::Mouse => "nothing (flip Joystick input to keyboard)".to_string(),
                    PortDevice::Joystick | PortDevice::Cd32Pad => {
                        "nothing (keyboard passes through to the Amiga)".to_string()
                    }
                    PortDevice::Analogue => {
                        "--pot-after scripting or the control protocol".to_string()
                    }
                    PortDevice::None => "nothing (empty port)".to_string(),
                }
            };
            format!("Port {} is driven by {}", port + 1, source)
        })
    }

    /// The value text shown on a row (the current enum/size/number; the file
    /// name or a placeholder for paths; On/Off for toggles).
    /// Whether the drive row's volume-name box applies: a name labels a
    /// directory mount's FFS volume, so a CD image (which attaches a
    /// CD-ROM drive) has nothing to name, and the WHDLoad paths mount
    /// under fixed volume names (WHDBoot:/WHDGame:).
    pub fn drive_name_applies(&self, field: LauncherField) -> bool {
        !field.is_whdload_path_field()
            && !self
                .path(field)
                .is_some_and(crate::config::is_cd_image_path)
    }

    /// Where a WHDLoad game's machine comes from: the slave's own header,
    /// or this configuration.
    pub fn whdload_machine(&self) -> crate::config::WhdloadMachine {
        self.whdload_machine
    }

    pub fn value_label(&self, field: LauncherField) -> String {
        fn enabled_label(on: bool) -> String {
            if on { "Enabled" } else { "Disabled" }.to_string()
        }
        match field {
            F::WhdloadMachine => match self.whdload_machine {
                crate::config::WhdloadMachine::Auto => "Auto".to_string(),
                crate::config::WhdloadMachine::Copperline => "Copperline".to_string(),
            },
            F::WhdloadEnabled => if self.whdload_enabled {
                "Enabled"
            } else {
                "Disabled"
            }
            .to_string(),
            F::Chipset => chipset_name(self.chipset).to_string(),
            F::Rtg => rtg_card_name(self.rtg).to_string(),
            F::Agnus => match self.agnus {
                None => "Auto".to_string(),
                Some(a) => agnus_name(a).to_string(),
            },
            F::Denise => match self.denise {
                None => "Auto".to_string(),
                Some(d) => denise_name(d).to_string(),
            },
            F::Video => video_name(self.video).to_string(),
            F::Cpu => cpu_name(self.cpu).to_string(),
            F::Clock => format_mhz(self.clock_mhz),
            F::ChipRam => size_label(self.chip_ram),
            F::FastRam => size_label(self.fast_ram),
            F::SlowRam => size_label(self.slow_ram),
            F::RamInit => match self.ram_init {
                RamInit::Zero => "Zero".to_string(),
                RamInit::Pattern { .. } => "Fixed".to_string(),
                RamInit::Random { .. } => "Random".to_string(),
            },
            F::RamPattern => format!("0x{:04X}", self.ram_pattern),
            F::MbRam => size_label(self.mb_ram),
            F::AccelRam => size_label(self.accel_ram),
            F::Z3Ram => size_label(self.z3_ram),
            F::FloppyDrives => self.floppy_drives.to_string(),
            F::FloppySpeed => crate::floppy::speed_label(self.floppy_speed),
            F::CdInsertDelay => {
                if self.cd_insert_delay <= 0.0 {
                    "At boot".to_string()
                } else {
                    format!("{:.0} s", self.cd_insert_delay)
                }
            }
            F::Overscan => match self.overscan {
                Overscan::Tv => "TV".to_string(),
                Overscan::Full => "Full".to_string(),
            },
            F::PixelAspect => match self.pixel_aspect {
                PixelAspect::Tv => "TV (4:3)".to_string(),
                PixelAspect::Square => "Square".to_string(),
            },
            F::Scaling => self.scaling.label().to_string(),
            F::Tint => self.tint.menu_label().to_string(),
            F::Bezel => self.bezel.menu_label().to_string(),
            F::MenuScale => self.menu_scale.menu_label().to_string(),
            F::Mt32Lcd => self.mt32_lcd.menu_label().to_string(),
            F::Mt32Panel => enabled_label(self.mt32_panel),
            F::Rtc => enabled_label(self.rtc),
            F::Identify => enabled_label(self.identify),
            F::Fpu => enabled_label(self.fpu),
            F::Icache => enabled_label(self.icache),
            F::Dcache => enabled_label(self.dcache),
            F::Jit => enabled_label(self.jit),
            F::StartFullscreen => enabled_label(self.start_fullscreen),
            F::ShowStatusBar => enabled_label(self.show_status_bar),
            F::PerfOverlay => enabled_label(self.perf_overlay),
            F::Deinterlace => enabled_label(self.deinterlace),
            F::FloppySounds => enabled_label(self.floppy_sounds),
            F::PowerOn => enabled_label(self.power_on),
            F::RealtimePriority => enabled_label(self.realtime_priority),
            F::Toccata => enabled_label(self.toccata),
            #[cfg(feature = "mhi")]
            F::Mhi => enabled_label(self.mhi),
            #[cfg(feature = "coppersynth")]
            F::CsynthPanel => enabled_label(self.csynth_panel),
            F::Phosphor => {
                if self.phosphor <= 0.0 {
                    "Disabled".to_string()
                } else {
                    format!("{:.2}", self.phosphor)
                }
            }
            F::BridgeDevice => match self.bridge_edit() {
                None => "(none)".to_string(),
                Some(_) if self.df_bridge_none[self.bridge_edit_drive] => "None".to_string(),
                Some(c) => c.driver.label().to_string(),
            },
            F::BridgePort => match self.bridge_edit().and_then(|c| c.port.clone()) {
                None => "Automatic".to_string(),
                Some(p) => p,
            },
            // Named as the drive's own jumpers are: A/B on an IBM PC cable,
            // DS0..DS3 on a Shugart one.
            F::BridgeCable => match self.bridge_edit().map(|c| c.cable) {
                Some(BridgeCable::DriveA) => "Drive A (IBM)".to_string(),
                Some(BridgeCable::DriveB) => "Drive B (IBM)".to_string(),
                Some(BridgeCable::Shugart0) => "DS0 (Shugart)".to_string(),
                Some(BridgeCable::Shugart1) => "DS1 (Shugart)".to_string(),
                Some(BridgeCable::Shugart2) => "DS2 (Shugart)".to_string(),
                Some(BridgeCable::Shugart3) => "DS3 (Shugart)".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeDensity => match self.bridge_edit().map(|c| c.density) {
                Some(BridgeDensity::Auto) => "Automatic".to_string(),
                Some(BridgeDensity::Dd) => "DD only".to_string(),
                Some(BridgeDensity::Hd) => "HD only".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeReadMode => match self.bridge_edit().map(|c| c.mode) {
                Some(BridgeReadMode::Compatible) => "Compatible".to_string(),
                Some(BridgeReadMode::Normal) => "Normal".to_string(),
                Some(BridgeReadMode::Stalling) => "Stalling".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeReplaySpeed => match self.bridge_edit().map_or(100, |c| c.speed) {
                200 => "Fast".to_string(),
                _ => "Normal".to_string(),
            },
            F::Shader => self.shader.kind().menu_label().to_string(),
            F::ShaderStrength => format!("{:.2}", self.shader_strength),
            F::FloppyVolume => format!("{}%", self.floppy_volume),
            F::PacingBudget => match self.pacing_budget {
                PacingBudget::Cycles => "Cycles".to_string(),
                PacingBudget::Instructions => "Instructions".to_string(),
            },
            F::Warp => self.warp.label().to_string(),
            F::Joystick => self.joystick_input_mode.menu_label().to_string(),
            F::MouseSensitivity => crate::config::mouse_sensitivity_label(self.mouse_sensitivity),
            F::MouseCapture => match self.mouse_capture {
                MouseCapture::Click => "On click".to_string(),
                MouseCapture::Auto => "Automatic".to_string(),
                MouseCapture::Manual => "Shortcut only".to_string(),
            },
            F::Port1Device => PortDevice::menu_label(self.port_devices[0]).to_string(),
            F::Port2Device => PortDevice::menu_label(self.port_devices[1]).to_string(),
            F::ScsiController => match self.scsi_controller {
                None => "None".to_string(),
                Some(ScsiController::A2091) => "A2091 (Z2)".to_string(),
                Some(ScsiController::A4091) => "A4091 (Z3)".to_string(),
                Some(ScsiController::A3000) => "A3000 (onboard)".to_string(),
            },
            F::LideBoard => match self.lide_board {
                None => "None".to_string(),
                Some(LidePersonality::Ripple) => "RIPPLE".to_string(),
                Some(LidePersonality::Ride) => "RIDE".to_string(),
                Some(LidePersonality::AtBus2008) => "AT-Bus 2008".to_string(),
            },
            #[cfg(feature = "midi")]
            F::SerialMode => match self.serial_mode {
                // "None" (matching the Parallel device selector) reads better
                // than "Off" for the no-connection state.
                SerialMode::Off => "None".to_string(),
                SerialMode::Stdout => "Stdout".to_string(),
                SerialMode::Midi => "MIDI".to_string(),
                SerialMode::Tcp => "TCP".to_string(),
                SerialMode::TcpConnect => "TCP connect".to_string(),
                SerialMode::Pty => "PTY".to_string(),
            },
            // The dial-out address has no default -- there is no host to
            // guess -- so an empty box says what it wants instead.
            #[cfg(feature = "midi")]
            F::SerialConnect => self
                .serial_connect
                .clone()
                .unwrap_or_else(|| "(host:port)".to_string()),
            // The listen address does have a default, so an empty box shows
            // the address the run would actually bind.
            #[cfg(feature = "midi")]
            F::SerialListen => self
                .serial_listen
                .clone()
                .unwrap_or_else(|| crate::config::SERIAL_TCP_DEFAULT_LISTEN.to_string()),
            #[cfg(feature = "midi")]
            F::MidiOut => {
                if self.midi_out_is_mt32() {
                    return crate::midi::MIDI_OUT_MT32_LABEL.to_string();
                }
                if self.midi_out_is_csynth() {
                    return crate::midi::MIDI_OUT_CSYNTH_LABEL.to_string();
                }
                self.midi_out.clone().unwrap_or_else(|| "None".to_string())
            }
            #[cfg(feature = "midi")]
            F::MidiIn => {
                #[cfg(feature = "mt32")]
                if crate::config::midi_out_is_mt32(self.midi_in.as_deref()) {
                    return crate::midi::MIDI_OUT_MT32_LABEL.to_string();
                }
                self.midi_in.clone().unwrap_or_else(|| "None".to_string())
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthSoundfont if self.csynth_soundfont.is_none() => {
                // The bundled bank, named rather than blank: an unset row
                // is not an empty setting, it is the default in force.
                "GeneralUser-GS".to_string()
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthMt32Mode => match self.csynth_mt32_mode.as_deref() {
                None => "Auto".to_string(),
                Some(m) if m.eq_ignore_ascii_case("on") => "On".to_string(),
                Some(m) if m.eq_ignore_ascii_case("off") => "Off".to_string(),
                Some(_) => "Auto".to_string(),
            },
            F::ParallelDevice => match self.parallel_device {
                ParallelDevice::None => "None".to_string(),
                ParallelDevice::Printer => "Printer".to_string(),
                ParallelDevice::Sampler => "Sampler".to_string(),
            },
            F::SamplerInput => self
                .sampler_input
                .clone()
                .unwrap_or_else(|| "Default".to_string()),
            F::SamplerGain => sampler_gain_label(self.sampler_gain_db),
            F::Ethernet => match self.a2065_net.as_ref() {
                None => "None".to_string(),
                Some(NetConfig::None) => "Isolated".to_string(),
                Some(NetConfig::Loopback) => "Loopback".to_string(),
                Some(NetConfig::Nat) => "NAT".to_string(),
                Some(NetConfig::Bridge { .. }) => "Bridged".to_string(),
            },
            F::EthernetInterface => match self.a2065_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => self
                    .bridge_interfaces
                    .iter()
                    .find(|(name, _)| name == interface)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| format!("{interface} (unavailable)")),
                _ => "—".to_string(),
            },
            F::HostSocket if self.hostsocket_host_mode => "Host".to_string(),
            F::HostSocket => match self.hostsocket_net.as_ref() {
                None => "None".to_string(),
                Some(NetConfig::None) => "Isolated".to_string(),
                Some(NetConfig::Loopback) => "Loopback".to_string(),
                Some(NetConfig::Nat) => "NAT".to_string(),
                Some(NetConfig::Bridge { .. }) => "Bridged".to_string(),
            },
            F::HostSocketInterface => match self.hostsocket_net.as_ref() {
                Some(NetConfig::Bridge { interface }) => self
                    .bridge_interfaces
                    .iter()
                    .find(|(name, _)| name == interface)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| format!("{interface} (unavailable)")),
                _ => "—".to_string(),
            },
            F::AudioDevice => self.audio_output.label().to_string(),
            F::AudioChannelMode => match self.audio_channel_mode {
                ChannelMode::Stereo => "Stereo".to_string(),
                ChannelMode::Mono => "Mono".to_string(),
            },
            F::AudioStereoSeparation => format!("{}%", self.audio_stereo_separation),
            F::AudioFilter => match self.audio_filter {
                AudioFilterMode::Auto => "Auto".to_string(),
                AudioFilterMode::On => "Enabled".to_string(),
                AudioFilterMode::Off => "Disabled".to_string(),
            },
            F::Filesys0Boot | F::Filesys1Boot | F::Filesys2Boot | F::Filesys3Boot => {
                let (slot, _) = filesys_slot(field).expect("boot field");
                match self.filesys_bootpri[slot] {
                    -128 => "Never".to_string(),
                    pri => pri.to_string(),
                }
            }
            F::IdeMasterBoot
            | F::IdeSlaveBoot
            | F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot
            | F::LideDrive0Boot
            | F::LideDrive1Boot
            | F::LideDrive2Boot
            | F::LideDrive3Boot => drive_bootpri_label(self.effective_bootpri(field)),
            F::Filesys0ReadOnly
            | F::Filesys1ReadOnly
            | F::Filesys2ReadOnly
            | F::Filesys3ReadOnly => {
                let slot = filesys_readonly_slot(field).expect("readonly field");
                if self.filesys_readonly[slot] {
                    "Read-only".to_string()
                } else {
                    "Read-write".to_string()
                }
            }
            // SCSI, IDE, and lide drive slots: flag CD images, which attach
            // a CD-ROM drive (SCSI or ATAPI) rather than a hard disk there.
            F::ScsiUnit0
            | F::ScsiUnit1
            | F::ScsiUnit2
            | F::ScsiUnit3
            | F::ScsiUnit4
            | F::ScsiUnit5
            | F::ScsiUnit6
            | F::IdeMaster
            | F::IdeSlave
            | F::LideDrive0
            | F::LideDrive1
            | F::LideDrive2
            | F::LideDrive3 => {
                let label = self.path_label(field, "(none)");
                match self.path(field) {
                    Some(p) if crate::config::is_cd_image_path(p) => format!("{label} (CD-ROM)"),
                    _ => label,
                }
            }
            // The WHDLoad directories all have a place they go when unset,
            // under the one WHDLoad directory (crate::paths::whdload_dir),
            // so their placeholder says the setting is doing something
            // rather than nothing. The game itself has no default, and the
            // two support archives are either there or not.
            F::WhdloadKickstarts | F::WhdloadLibrary | F::WhdloadGames => {
                self.path_label(field, "(default)")
            }
            F::WhdloadWhdPackage | F::WhdloadSkickPackage => self.path_label(field, "(none)"),
            // Path/drive fields: the file name, or a placeholder.
            F::Rom => self.path_label(field, "(bundled AROS)"),
            // The A4091 autoboots from a bundled open-source ROM when no
            // image names one; the other controllers have no such default.
            F::ScsiRom if self.scsi_controller_is_a4091() => {
                self.path_label(field, "(bundled A4091 ROM)")
            }
            _ if rows_contains_kind(field, RowKind::Path)
                || rows_contains_kind(field, RowKind::Drive)
                || rows_contains_kind(field, RowKind::FloppyMedia) =>
            {
                self.path_label(field, "(none)")
            }
            // Toggles
            _ => {
                if self.toggle_value(field) {
                    "On".to_string()
                } else {
                    "Off".to_string()
                }
            }
        }
    }

    /// Change the fixed power-on word and make it the active policy. The text
    /// box only applies in Fixed mode, but setting both here keeps this method
    /// correct if another frontend reuses it directly.
    pub fn set_ram_pattern(&mut self, word: u16) {
        self.ram_pattern = word;
        self.ram_init = RamInit::Pattern { word };
    }

    fn path_label(&self, field: LauncherField, empty: &str) -> String {
        match self.path(field) {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => empty.to_string(),
        }
    }

    /// The shader picker's options: the built-in presets, plus the user
    /// shader the loaded config named. There is no file browser for shaders
    /// here, so Custom is offered only when a path came in with the config.
    fn shader_options(&self) -> Vec<ShaderMode> {
        let mut options = vec![
            ShaderMode::None,
            ShaderMode::Scanlines,
            ShaderMode::Mask,
            ShaderMode::Crt,
        ];
        if let Some(path) = &self.shader_custom {
            options.push(ShaderMode::Custom(path.clone()));
        }
        options
    }

    /// Like [`cycle_slice`], but for the non-`Copy` shader options.
    fn cycled_shader(&self, forward: bool) -> ShaderMode {
        let options = self.shader_options();
        let n = options.len();
        let idx = options.iter().position(|m| *m == self.shader).unwrap_or(0);
        let next = if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        };
        options[next].clone()
    }

    /// Step a cycle/stepper field forward (`forward`) or backward.
    pub fn cycle(&mut self, field: LauncherField, forward: bool) {
        match field {
            F::WhdloadEnabled => {
                self.whdload_enabled = !self.whdload_enabled;
                let _ = forward;
            }
            F::WhdloadMachine => {
                use crate::config::WhdloadMachine as M;
                self.whdload_machine = match self.whdload_machine {
                    M::Auto => M::Copperline,
                    M::Copperline => M::Auto,
                };
                let _ = forward;
            }
            F::Chipset => self.chipset = cycle_slice(&CHIPSETS, self.chipset, forward),
            F::Rtg => {
                // The Zorro III cards sit at the list's tail so a 24-bit CPU
                // can cycle everything before them (the Zorro II cards).
                let cards = if cpu_is_32bit(self.cpu) {
                    &RTG_CARDS[..]
                } else {
                    &RTG_CARDS[..4]
                };
                self.rtg = cycle_slice(cards, self.rtg, forward);
            }
            F::Agnus => self.agnus = cycle_slice(&AGNUS_CHOICES, self.agnus, forward),
            F::Denise => self.denise = cycle_slice(&DENISE_CHOICES, self.denise, forward),
            F::Video => self.video = cycle_slice(&VIDEO_CHOICES, self.video, forward),
            F::Cpu => {
                self.cpu = cycle_slice(&CPUS, self.cpu, forward);
                // Re-derive the CPU-dependent toggles for the new part, as if
                // the model had been picked fresh (the panel greys whichever
                // do not apply).
                self.fpu = self.cpu.default_fpu();
                self.icache = self.cpu.has_instruction_cache();
                self.dcache = self.cpu.has_data_cache();
                self.clock_mhz = self.cpu.default_clock_mhz();
                if !cpu_is_32bit(self.cpu) {
                    // Zorro III RAM, motherboard RAM, accelerator RAM, and
                    // the Zorro III RTG cards all sit beyond a 24-bit bus;
                    // dropping them (rather than just greying their rows)
                    // keeps the emitted config launchable. Picasso II/II+ and
                    // Graffity [Zorro II] remain fitted (Zorro II cards).
                    self.z3_ram = 0;
                    self.mb_ram = 0;
                    self.accel_ram = 0;
                    if matches!(self.rtg, RtgCard::Z3660 | RtgCard::GraffityZ3) {
                        self.rtg = RtgCard::None;
                    }
                }
            }
            F::Clock => self.clock_mhz = cycle_floats(&CLOCK_PRESETS, self.clock_mhz, forward),
            F::ChipRam => self.chip_ram = cycle_slice(&CHIP_PRESETS, self.chip_ram, forward),
            F::FastRam => self.fast_ram = cycle_nearest(&FAST_PRESETS, self.fast_ram, forward),
            F::SlowRam => self.slow_ram = cycle_nearest(&SLOW_PRESETS, self.slow_ram, forward),
            F::RamInit => {
                self.ram_init = match (self.ram_init, forward) {
                    (RamInit::Zero, true) | (RamInit::Pattern { .. }, false) => RamInit::Random {
                        seed: self.ram_random_seed,
                    },
                    (RamInit::Random { .. }, true) | (RamInit::Zero, false) => RamInit::Pattern {
                        word: self.ram_pattern,
                    },
                    (RamInit::Pattern { .. }, true) | (RamInit::Random { .. }, false) => {
                        RamInit::Zero
                    }
                };
            }
            F::MbRam => {
                // Only the A4000's Ramsey-07 extends past its four banks
                // into the $04000000-$06FFFFFF expansion space.
                let presets: &[usize] = if self.model == Some(MachineModel::A4000) {
                    &MB_PRESETS_A4000
                } else {
                    &MB_PRESETS
                };
                self.mb_ram = cycle_nearest(presets, self.mb_ram, forward);
            }
            F::AccelRam => self.accel_ram = cycle_nearest(&ACCEL_PRESETS, self.accel_ram, forward),
            F::Z3Ram => self.z3_ram = cycle_nearest(&Z3_PRESETS, self.z3_ram, forward),
            F::FloppyDrives => {
                self.floppy_drives = step_u8(self.floppy_drives, forward, 1, 4);
                // A bay that is no longer fitted has no business holding a
                // physical drive open: the row is gone from the page, so
                // nothing would say why the interface was busy the next time
                // it was asked for.
                #[cfg(feature = "fluxbridge")]
                for bay in self.df_bridge.iter_mut().skip(self.floppy_drives as usize) {
                    *bay = None;
                }
            }
            F::FloppySpeed => {
                self.floppy_speed = cycle_slice(&FLOPPY_SPEEDS, self.floppy_speed, forward)
            }
            F::CdInsertDelay => {
                let secs = self.cd_insert_delay + if forward { 1.0 } else { -1.0 };
                self.cd_insert_delay = secs.clamp(0.0, 60.0);
            }
            F::Phosphor => {
                let p = self.phosphor + if forward { 0.05 } else { -0.05 };
                // Snap to the 0.05 grid to avoid float drift accumulating.
                self.phosphor = (p.clamp(0.0, 0.95) * 20.0).round() / 20.0;
            }
            F::Shader => self.shader = self.cycled_shader(forward),
            F::ShaderStrength => {
                let s = self.shader_strength + if forward { 0.1 } else { -0.1 };
                // Snap to the 0.1 grid to avoid float drift accumulating.
                self.shader_strength = (s.clamp(0.0, 1.0) * 10.0).round() / 10.0;
            }
            F::FloppyVolume => self.floppy_volume = step_u8(self.floppy_volume, forward, 0, 100),
            F::Overscan => self.overscan = cycle_slice(&OVERSCANS, self.overscan, forward),
            F::Tint => self.tint = cycle_slice(&TINTS, self.tint, forward),
            F::Bezel => self.bezel = cycle_slice(&BezelStyle::MENU_ORDER, self.bezel, forward),
            F::MenuScale => {
                self.menu_scale = cycle_slice(&MenuScale::MENU_ORDER, self.menu_scale, forward);
            }
            F::Mt32Lcd => {
                self.mt32_lcd = cycle_slice(&Mt32Lcd::MENU_ORDER, self.mt32_lcd, forward);
            }
            // Two states cycle the same either way round.
            F::Mt32Panel => self.mt32_panel = !self.mt32_panel,
            F::Rtc => self.rtc = !self.rtc,
            F::Identify => self.identify = !self.identify,
            F::Fpu => self.fpu = !self.fpu,
            F::Icache => self.icache = !self.icache,
            F::Dcache => self.dcache = !self.dcache,
            F::Jit => self.jit = !self.jit,
            F::StartFullscreen => self.start_fullscreen = !self.start_fullscreen,
            F::ShowStatusBar => self.show_status_bar = !self.show_status_bar,
            F::PerfOverlay => self.perf_overlay = !self.perf_overlay,
            F::Deinterlace => self.deinterlace = !self.deinterlace,
            F::FloppySounds => self.floppy_sounds = !self.floppy_sounds,
            F::PowerOn => self.power_on = !self.power_on,
            F::RealtimePriority => self.realtime_priority = !self.realtime_priority,
            F::Toccata => self.toccata = !self.toccata,
            #[cfg(feature = "mhi")]
            F::Mhi => self.mhi = !self.mhi,
            #[cfg(feature = "coppersynth")]
            F::CsynthPanel => self.csynth_panel = !self.csynth_panel,
            F::PixelAspect => {
                self.pixel_aspect = cycle_slice(&PIXEL_ASPECTS, self.pixel_aspect, forward)
            }
            F::Scaling => {
                self.scaling = cycle_slice(&DisplayScaling::MENU_ORDER, self.scaling, forward)
            }
            F::PacingBudget => {
                self.pacing_budget = cycle_slice(&PACINGS, self.pacing_budget, forward)
            }
            F::Warp => self.warp = cycle_slice(&WARPS, self.warp, forward),
            F::Joystick => {
                self.joystick_input_mode =
                    cycle_slice(&JOYSTICK_MODES, self.joystick_input_mode, forward)
            }
            F::MouseSensitivity => {
                self.mouse_sensitivity = if forward {
                    self.mouse_sensitivity.saturating_add(1).min(100)
                } else {
                    self.mouse_sensitivity.saturating_sub(1)
                }
            }
            F::MouseCapture => {
                self.mouse_capture = cycle_slice(&MOUSE_CAPTURES, self.mouse_capture, forward)
            }
            F::Port1Device => {
                self.port_devices[0] = cycle_slice(&PORT_DEVICES, self.port_devices[0], forward)
            }
            F::Port2Device => {
                self.port_devices[1] = cycle_slice(&PORT_DEVICES, self.port_devices[1], forward)
            }
            F::ScsiController => {
                // The motherboard SCSI is only on offer where the silicon is.
                let choices: Vec<Option<ScsiController>> = SCSI_CONTROLLERS
                    .into_iter()
                    .filter(|c| self.has_sdmac() || *c != Some(ScsiController::A3000))
                    .collect();
                self.scsi_controller = cycle_slice(&choices, self.scsi_controller, forward);
                self.drop_unreachable_host_disks();
            }
            F::LideBoard => {
                self.lide_board = cycle_slice(&LIDE_BOARDS, self.lide_board, forward);
                // Drop drives beyond the new board's channel count, so a
                // RIPPLE-only channel 1 drive does not linger unreachable
                // (and unrepresentable -- `[lide] drives` is positional)
                // behind a board that no longer has that channel.
                if let Some(board) = self.lide_board {
                    for slot in board.max_drives()..self.lide_drives.len() {
                        self.lide_drives[slot] = None;
                        self.lide_drive_names[slot] = None;
                        self.lide_drive_bootpri[slot] = None;
                        self.lide_drive_boot_off[slot] = false;
                    }
                }
                // A real host disk on a channel the new personality lacks
                // (e.g. RIPPLE channel 1 -> RIDE/AT-Bus 2008) is just as
                // unreachable as an image drive there, and just as invisible
                // if left attached -- drop it the same way ScsiController's
                // handler above does for its own board switch.
                self.drop_unreachable_host_disks();
            }
            #[cfg(feature = "midi")]
            F::SerialMode => {
                // Every mode is on offer: choosing tcp-connect brings its
                // Connect box with it, so the address the mode needs can be
                // typed here rather than only in a hand-written config.
                self.serial_mode = cycle_slice(&SERIAL_MODES, self.serial_mode, forward)
            }
            #[cfg(feature = "midi")]
            F::MidiOut => {
                // The built-in synths ride at the end of the output
                // list: always there to be chosen, whatever the host
                // offers -- the MT-32 first, then Coppersynth.
                let names: Vec<String> = self
                    .midi_endpoints
                    .outputs
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(mt32_endpoint(true))
                    .chain(csynth_endpoint(true))
                    .collect();
                self.midi_out =
                    crate::midi::next_endpoint(self.midi_out.as_deref(), &names, forward);
                // The MT-32 is only a source while it is the destination,
                // so moving the output elsewhere takes the input with it.
                #[cfg(feature = "mt32")]
                if !self.midi_out_is_mt32()
                    && crate::config::midi_out_is_mt32(self.midi_in.as_deref())
                {
                    self.midi_in = None;
                }
            }
            #[cfg(feature = "midi")]
            F::MidiIn => {
                // The module is a sound module: it has no keyboard, and
                // what it sends is an answer to what it was sent. So it is
                // offered as a source only while it is the destination,
                // which is also the wiring a patch editor needs.
                let names: Vec<String> = self
                    .midi_endpoints
                    .inputs
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(mt32_endpoint(self.midi_out_is_mt32()))
                    .collect();
                self.midi_in = crate::midi::next_endpoint(self.midi_in.as_deref(), &names, forward);
            }
            #[cfg(feature = "coppersynth")]
            F::CsynthMt32Mode => {
                // Auto -> On -> Off, stored as the config spells it, with
                // Auto stored as unset so an untouched row emits nothing.
                let next = match self.csynth_mt32_mode.as_deref() {
                    None => Some("on"),
                    Some(m) if m.eq_ignore_ascii_case("on") => Some("off"),
                    Some(m) if m.eq_ignore_ascii_case("off") => None,
                    Some(_) => Some("on"),
                };
                let next = if forward {
                    next
                } else {
                    // The same ring walked the other way.
                    match self.csynth_mt32_mode.as_deref() {
                        None => Some("off"),
                        Some(m) if m.eq_ignore_ascii_case("off") => Some("on"),
                        Some(m) if m.eq_ignore_ascii_case("on") => None,
                        Some(_) => None,
                    }
                };
                self.csynth_mt32_mode = next.map(str::to_string);
            }
            F::ParallelDevice => {
                // None -> Printer -> Sampler. Selecting Printer reveals its
                // Output file row (with a Browse button); until a file is set
                // the printer is not persisted or attached (see to_raw).
                const DEVICES: [ParallelDevice; 3] = [
                    ParallelDevice::None,
                    ParallelDevice::Printer,
                    ParallelDevice::Sampler,
                ];
                self.parallel_device = cycle_slice(&DEVICES, self.parallel_device, forward);
            }
            F::SamplerInput => {
                // Re-read on each step so a device connected since the screen
                // opened appears; on-demand only, so no background polling.
                self.refresh_sampler_inputs();
                self.sampler_input = crate::sampler::next_input_device(
                    self.sampler_input.as_deref(),
                    &self.sampler_input_devices,
                    forward,
                );
            }
            F::SamplerGain => {
                self.sampler_gain_db =
                    cycle_floats(&SAMPLER_GAIN_STEPS, self.sampler_gain_db as f64, forward) as f32;
            }
            F::Ethernet => {
                cycle_net_board(&mut self.a2065_net, &self.bridge_interfaces, forward);
            }
            F::EthernetInterface => {
                cycle_bridge_interface(&mut self.a2065_net, &self.bridge_interfaces, forward);
            }
            F::HostSocket => {
                cycle_hostsocket_board(
                    &mut self.hostsocket_net,
                    &mut self.hostsocket_host_mode,
                    &self.bridge_interfaces,
                    forward,
                );
            }
            F::HostSocketInterface => {
                cycle_bridge_interface(&mut self.hostsocket_net, &self.bridge_interfaces, forward);
            }
            F::AudioDevice => {
                // Re-read on each step so a device connected since the screen
                // opened appears; on-demand only, so no background polling.
                self.refresh_audio_devices();
                self.audio_output = self.audio_output.cycle(&self.audio_devices, forward);
            }
            F::AudioChannelMode => {
                self.audio_channel_mode = match self.audio_channel_mode {
                    ChannelMode::Stereo => ChannelMode::Mono,
                    ChannelMode::Mono => ChannelMode::Stereo,
                }
            }
            F::AudioFilter => {
                self.audio_filter = cycle_slice(&AUDIO_FILTER_MODES, self.audio_filter, forward)
            }
            F::BridgeDevice => {
                // "None" sits before the first driver in the cycle: from it,
                // forward reaches the first interface, backward the last.
                let drivers = bridge_drivers();
                let bay = self.bridge_edit_drive;
                if self.bridge_edit().is_some() && !drivers.is_empty() {
                    if self.df_bridge_none[bay] {
                        self.df_bridge_none[bay] = false;
                        let end = if forward {
                            drivers[0]
                        } else {
                            drivers[drivers.len() - 1]
                        };
                        if let Some(c) = self.bridge_edit_mut() {
                            c.driver = end;
                        }
                    } else {
                        let (first, last) = (drivers[0], drivers[drivers.len() - 1]);
                        let at_edge = self
                            .bridge_edit()
                            .is_some_and(|c| c.driver == if forward { last } else { first });
                        if at_edge {
                            self.df_bridge_none[bay] = true;
                        } else if let Some(c) = self.bridge_edit_mut() {
                            c.driver = cycle_slice(&drivers, c.driver, forward);
                        }
                    }
                }
            }
            F::BridgePort => {
                let options = self.bridge_port_options();
                if let Some(c) = self.bridge_edit_mut() {
                    let idx = options.iter().position(|p| *p == c.port).unwrap_or(0);
                    let n = options.len();
                    let next = if forward {
                        (idx + 1) % n
                    } else {
                        (idx + n - 1) % n
                    };
                    c.port = options[next].clone();
                }
            }
            F::BridgeCable => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.cable = cycle_slice(&BRIDGE_CABLES, c.cable, forward);
                }
            }
            F::BridgeDensity => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.density = cycle_slice(&BRIDGE_DENSITIES, c.density, forward);
                }
            }
            F::BridgeReadMode => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.mode = cycle_slice(&BRIDGE_READ_MODES, c.mode, forward);
                }
            }
            F::BridgeReplaySpeed => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.speed = cycle_slice(&BRIDGE_REPLAY_SPEEDS, c.speed, forward);
                }
            }
            F::AudioStereoSeparation => {
                self.audio_stereo_separation = cycle_nearest(
                    &STEREO_SEPARATION_STEPS,
                    usize::from(self.audio_stereo_separation),
                    forward,
                ) as u8
            }
            F::IdeMasterBoot
            | F::IdeSlaveBoot
            | F::ScsiUnit0Boot
            | F::ScsiUnit1Boot
            | F::ScsiUnit2Boot
            | F::ScsiUnit3Boot
            | F::ScsiUnit4Boot
            | F::ScsiUnit5Boot
            | F::ScsiUnit6Boot
            | F::LideDrive0Boot
            | F::LideDrive1Boot
            | F::LideDrive2Boot
            | F::LideDrive3Boot => {
                // The arrows only move a live priority; a drive whose Bootable
                // box is cleared shows its number greyed and does not step.
                if !self.drive_boot_off(field) {
                    self.set_drive_bootpri(
                        field,
                        step_drive_bootpri(self.drive_bootpri(field), forward),
                    );
                }
            }
            _ => {
                if let Some((slot, true)) = filesys_slot(field) {
                    self.filesys_bootpri[slot] = cycle_bootpri(self.filesys_bootpri[slot], forward);
                } else if let Some(slot) = filesys_readonly_slot(field) {
                    // Two values: either direction lands on the other one.
                    self.filesys_readonly[slot] = !self.filesys_readonly[slot];
                }
            }
        }
    }

    /// Flip a toggle field (no-op if the field is not a toggle).
    pub fn toggle(&mut self, field: LauncherField) {
        match field {
            F::Df0WriteProtect | F::Df1WriteProtect | F::Df2WriteProtect | F::Df3WriteProtect
                if Self::drive_protect_bay(field).is_some() =>
            {
                let bay = Self::drive_protect_bay(field).expect("checked above");
                self.df_write_protected[bay] = !self.df_write_protected[bay];
                if let Some(bridge) = self.df_bridge[bay].as_mut() {
                    bridge.write_protected = self.df_write_protected[bay];
                }
            }
            F::Df0WriteProtect => self.df_write_protected[0] = !self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1] = !self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2] = !self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3] = !self.df_write_protected[3],
            _ => {}
        }
    }

    /// Set a path field's value (a floppy image replaces that drive's
    /// playlist with a single disk and wires the drive in).
    pub fn set_path(&mut self, field: LauncherField, path: PathBuf) {
        // Adding a hard-disk image to an empty slot seeds its boot priority from
        // the positional cascade, so drives added in the launcher (with no
        // config priorities of their own) do not all tie at 0. A slot that
        // already held an image keeps whatever priority it had.
        // A Paths row is a host preference, not part of the machine, and
        // shares nothing with the rest of this: it stores its directory and
        // saves, and none of the drive bookkeeping below applies.
        if field.is_paths_field() {
            self.set_paths_dir(field, path);
            self.apply_paths();
            return;
        }
        let seed_cascade = Self::is_drive_field(field)
            && self.path(field).is_none()
            && !crate::config::is_cd_image_path(&path);
        match field {
            F::Rom => self.rom = Some(path),
            F::Mt32ControlRom => self.mt32_control_rom = Some(path),
            #[cfg(feature = "coppersynth")]
            F::CsynthSoundfont => self.csynth_soundfont = Some(path),
            F::Mt32PcmRom => self.mt32_pcm_rom = Some(path),
            F::ExtendedRom => self.extended_rom = Some(path),
            F::Df0Image => self.set_floppy(0, path),
            F::Df1Image => self.set_floppy(1, path),
            F::Df2Image => self.set_floppy(2, path),
            F::Df3Image => self.set_floppy(3, path),
            F::IdeMaster => self.ide_master = Some(path),
            F::IdeSlave => self.ide_slave = Some(path),
            F::ScsiRom => self.scsi_rom = Some(path),
            F::ScsiRomOdd => self.scsi_rom_odd = Some(path),
            F::ScsiUnit0 => self.scsi_units[0] = Some(path),
            F::ScsiUnit1 => self.scsi_units[1] = Some(path),
            F::ScsiUnit2 => self.scsi_units[2] = Some(path),
            F::ScsiUnit3 => self.scsi_units[3] = Some(path),
            F::ScsiUnit4 => self.scsi_units[4] = Some(path),
            F::ScsiUnit5 => self.scsi_units[5] = Some(path),
            F::ScsiUnit6 => self.scsi_units[6] = Some(path),
            F::LideRom => self.lide_rom = Some(path),
            F::LideRomBank2 => self.lide_rom_bank2 = Some(path),
            F::LideDrive0 => self.lide_drives[0] = Some(path),
            F::LideDrive1 => self.lide_drives[1] = Some(path),
            F::LideDrive2 => self.lide_drives[2] = Some(path),
            F::LideDrive3 => self.lide_drives[3] = Some(path),
            F::CdImage => self.cd_image = Some(path),
            F::Cd32Nvram => self.cd32_nvram = Some(path),
            F::ParallelOutput => self.parallel_output = Some(path),
            F::WhdloadGame => self.whdload_game = Some(path),
            F::WhdloadKickstarts => self.whdload_kickstarts = Some(path),
            F::WhdloadLibrary => self.whdload_library = Some(path),
            F::WhdloadWhdPackage => self.whdload_whd_package = Some(path),
            F::WhdloadSkickPackage => self.whdload_skick_package = Some(path),
            F::WhdloadGames => self.whdload_games = Some(path),
            _ => {
                if let Some((slot, false)) = filesys_slot(field) {
                    self.filesys_dirs[slot] = Some(path);
                }
            }
        }
        self.refresh_drive_is_dir(field);
        if seed_cascade {
            if let Some(boot) = drive_boot_field(field) {
                if self.drive_bootpri(boot).is_none() && !self.drive_boot_off(boot) {
                    self.set_drive_bootpri(boot, hdd_boot_cascade(self.hdd_boot_rank(boot)));
                }
            }
        }
    }

    fn set_floppy(&mut self, idx: usize, path: PathBuf) {
        self.df_playlists[idx] = vec![path];
        // Wire the drive in if it was beyond the configured count.
        self.floppy_drives = self.floppy_drives.max(idx as u8 + 1);
    }

    /// Clear a path field's value.
    pub fn clear_path(&mut self, field: LauncherField) {
        // Cleared, a Paths row inherits again rather than pointing
        // nowhere: the entry goes out of `[paths]` entirely, so it
        // follows the default from then on instead of freezing today's.
        if let Some(entry) = Self::paths_entry(&mut self.paths, field) {
            *entry = None;
            self.apply_paths();
            return;
        }
        match field {
            F::Rom => self.rom = None,
            F::ExtendedRom => self.extended_rom = None,
            F::Mt32ControlRom => self.mt32_control_rom = None,
            #[cfg(feature = "coppersynth")]
            F::CsynthSoundfont => self.csynth_soundfont = None,
            F::Mt32PcmRom => self.mt32_pcm_rom = None,
            F::Df0Image => self.df_playlists[0].clear(),
            F::Df1Image => self.df_playlists[1].clear(),
            F::Df2Image => self.df_playlists[2].clear(),
            F::Df3Image => self.df_playlists[3].clear(),
            F::IdeMaster => self.ide_master = None,
            F::IdeSlave => self.ide_slave = None,
            F::ScsiRom => self.scsi_rom = None,
            F::ScsiRomOdd => self.scsi_rom_odd = None,
            F::ScsiUnit0 => self.scsi_units[0] = None,
            F::ScsiUnit1 => self.scsi_units[1] = None,
            F::ScsiUnit2 => self.scsi_units[2] = None,
            F::ScsiUnit3 => self.scsi_units[3] = None,
            F::ScsiUnit4 => self.scsi_units[4] = None,
            F::ScsiUnit5 => self.scsi_units[5] = None,
            F::ScsiUnit6 => self.scsi_units[6] = None,
            F::LideRom => self.lide_rom = None,
            F::LideRomBank2 => self.lide_rom_bank2 = None,
            F::LideDrive0 => self.lide_drives[0] = None,
            F::LideDrive1 => self.lide_drives[1] = None,
            F::LideDrive2 => self.lide_drives[2] = None,
            F::LideDrive3 => self.lide_drives[3] = None,
            F::CdImage => self.cd_image = None,
            F::Cd32Nvram => self.cd32_nvram = None,
            F::ParallelOutput => self.parallel_output = None,
            F::WhdloadGame => self.whdload_game = None,
            F::WhdloadKickstarts => self.whdload_kickstarts = None,
            F::WhdloadWhdPackage => self.whdload_whd_package = None,
            F::WhdloadSkickPackage => self.whdload_skick_package = None,
            F::WhdloadGames => self.whdload_games = None,
            F::WhdloadLibrary => self.whdload_library = None,
            _ => {
                if let Some((slot, false)) = filesys_slot(field) {
                    self.filesys_dirs[slot] = None;
                    // Boot priority or read-only on a mount with no directory is
                    // meaningless; reset both so a cleared slot emits nothing.
                    self.filesys_bootpri[slot] = -128;
                    self.filesys_readonly[slot] = false;
                }
            }
        }
        // A drive's volume name, filesystem, and boot priority are
        // meaningless once its image is gone.
        if Self::is_drive_field(field) {
            self.set_drive_name(field, String::new());
            self.set_drive_filesystem(field, crate::diskimage::FileSystem::FFS);
            self.clear_drive_bootpri(field);
            self.refresh_drive_is_dir(field);
        }
        // `[lide] drives` is a positional list in the config file -- a hole
        // cannot be represented -- so clearing a slot also clears every slot
        // after it, keeping the array always representable as a config.
        if let Some(i) = lide_drive_index(field) {
            for slot in i + 1..self.lide_drives.len() {
                self.lide_drives[slot] = None;
                self.lide_drive_names[slot] = None;
                self.lide_drive_bootpri[slot] = None;
                self.lide_drive_boot_off[slot] = false;
            }
        }
    }

    /// Reset a hard-disk drive's boot priority to unset (shown as 0) and its
    /// Bootable flag back on. Called when clearing the image (the priority has
    /// nothing to attach to). `field` is either the drive field or its
    /// boot-priority twin.
    fn clear_drive_bootpri(&mut self, field: LauncherField) {
        use LauncherField as F;
        match field {
            F::IdeMaster | F::IdeMasterBoot => {
                self.ide_master_bootpri = None;
                self.ide_master_boot_off = false;
            }
            F::IdeSlave | F::IdeSlaveBoot => {
                self.ide_slave_bootpri = None;
                self.ide_slave_boot_off = false;
            }
            _ => {
                if let Some(i) = scsi_boot_index(field) {
                    self.scsi_unit_bootpri[i] = None;
                    self.scsi_unit_boot_off[i] = false;
                } else if let Some(i) = lide_drive_index(field) {
                    self.lide_drive_bootpri[i] = None;
                    self.lide_drive_boot_off[i] = false;
                }
            }
        }
    }

    /// What a hard-disk drive slot holds, if anything: an image path, or a
    /// real host disk. Boot priority applies to either -- a real disk takes
    /// its place in the boot order the same way an image does.
    fn drive_holds(&self, drive: LauncherField) -> Option<DriveContents<'_>> {
        if self.host_disk_on_row(drive).is_some() {
            return Some(DriveContents::HostDisk);
        }
        self.path(drive).map(DriveContents::Image)
    }

    /// The hard-disk drive field a boot-priority field belongs to (its twin on
    /// the Storage tab), or None when `field` is not a boot-priority field.
    fn boot_field_drive(field: LauncherField) -> Option<LauncherField> {
        use LauncherField as F;
        Some(match field {
            F::IdeMasterBoot => F::IdeMaster,
            F::IdeSlaveBoot => F::IdeSlave,
            F::ScsiUnit0Boot => F::ScsiUnit0,
            F::ScsiUnit1Boot => F::ScsiUnit1,
            F::ScsiUnit2Boot => F::ScsiUnit2,
            F::ScsiUnit3Boot => F::ScsiUnit3,
            F::ScsiUnit4Boot => F::ScsiUnit4,
            F::ScsiUnit5Boot => F::ScsiUnit5,
            F::ScsiUnit6Boot => F::ScsiUnit6,
            F::LideDrive0Boot => F::LideDrive0,
            F::LideDrive1Boot => F::LideDrive1,
            F::LideDrive2Boot => F::LideDrive2,
            F::LideDrive3Boot => F::LideDrive3,
            _ => return None,
        })
    }

    /// The stored priority for a boot-priority field, apart from the Bootable
    /// flag: `None` is unset (shown as 0). Never the -128 sentinel -- that is
    /// [`Self::drive_boot_off`].
    fn drive_bootpri(&self, field: LauncherField) -> Option<i8> {
        use LauncherField as F;
        match field {
            F::IdeMasterBoot => self.ide_master_bootpri,
            F::IdeSlaveBoot => self.ide_slave_bootpri,
            F::ScsiUnit0Boot => self.scsi_unit_bootpri[0],
            F::ScsiUnit1Boot => self.scsi_unit_bootpri[1],
            F::ScsiUnit2Boot => self.scsi_unit_bootpri[2],
            F::ScsiUnit3Boot => self.scsi_unit_bootpri[3],
            F::ScsiUnit4Boot => self.scsi_unit_bootpri[4],
            F::ScsiUnit5Boot => self.scsi_unit_bootpri[5],
            F::ScsiUnit6Boot => self.scsi_unit_bootpri[6],
            _ => lide_drive_index(field).and_then(|i| self.lide_drive_bootpri[i]),
        }
    }

    /// Store a drive's priority; `None` is unset (shown as 0, written as no key).
    pub fn set_drive_bootpri(&mut self, field: LauncherField, value: Option<i8>) {
        use LauncherField as F;
        match field {
            F::IdeMasterBoot => self.ide_master_bootpri = value,
            F::IdeSlaveBoot => self.ide_slave_bootpri = value,
            F::ScsiUnit0Boot => self.scsi_unit_bootpri[0] = value,
            F::ScsiUnit1Boot => self.scsi_unit_bootpri[1] = value,
            F::ScsiUnit2Boot => self.scsi_unit_bootpri[2] = value,
            F::ScsiUnit3Boot => self.scsi_unit_bootpri[3] = value,
            F::ScsiUnit4Boot => self.scsi_unit_bootpri[4] = value,
            F::ScsiUnit5Boot => self.scsi_unit_bootpri[5] = value,
            F::ScsiUnit6Boot => self.scsi_unit_bootpri[6] = value,
            _ => {
                if let Some(i) = lide_drive_index(field) {
                    self.lide_drive_bootpri[i] = value;
                }
            }
        }
    }

    /// Whether a boot-priority field is editable: its drive holds a hard-disk
    /// image. Priority is meaningless for an empty slot or a CD image.
    fn boot_field_applies(&self, field: LauncherField) -> bool {
        match Self::boot_field_drive(field).and_then(|drive| self.drive_holds(drive)) {
            Some(DriveContents::Image(p)) => !crate::config::is_cd_image_path(p),
            Some(DriveContents::HostDisk) => false,
            None => false,
        }
    }

    /// Whether any Boot Priority row is editable -- a hard-disk drive is present.
    /// The page's info text is hidden when it is not, since there is nothing to
    /// manage.
    pub fn has_boot_priority_rows(&self) -> bool {
        BOOTPRI_ROWS
            .iter()
            .any(|r| self.boot_field_applies(r.field))
    }

    /// Whether a drive's Bootable box is cleared -- the config's -128 sentinel,
    /// which mounts the volume but keeps it out of the boot vote.
    pub fn drive_boot_off(&self, field: LauncherField) -> bool {
        use LauncherField as F;
        match field {
            F::IdeMasterBoot => self.ide_master_boot_off,
            F::IdeSlaveBoot => self.ide_slave_boot_off,
            _ => {
                scsi_boot_index(field).is_some_and(|i| self.scsi_unit_boot_off[i])
                    || lide_drive_index(field).is_some_and(|i| self.lide_drive_boot_off[i])
            }
        }
    }

    /// The value the config stores for a drive: the -128 sentinel when its
    /// Bootable box is cleared, otherwise its priority (unset omits the key).
    fn effective_bootpri(&self, field: LauncherField) -> Option<i8> {
        if self.drive_boot_off(field) {
            Some(BOOT_PRI_NEVER)
        } else {
            self.drive_bootpri(field)
        }
    }

    /// Flip a drive's Bootable box. Clearing it shows the -128 sentinel the
    /// config will store, while keeping the priority underneath so re-ticking
    /// restores it within the session.
    pub fn toggle_drive_boot(&mut self, field: LauncherField) {
        let off = self.drive_boot_off(field);
        self.set_drive_boot_off(field, !off);
    }

    /// Set a drive's Bootable box (cleared = the -128 sentinel).
    fn set_drive_boot_off(&mut self, field: LauncherField, off: bool) {
        use LauncherField as F;
        match field {
            F::IdeMasterBoot => self.ide_master_boot_off = off,
            F::IdeSlaveBoot => self.ide_slave_boot_off = off,
            _ => {
                if let Some(i) = scsi_boot_index(field) {
                    self.scsi_unit_boot_off[i] = off;
                } else if let Some(i) = lide_drive_index(field) {
                    self.lide_drive_boot_off[i] = off;
                }
            }
        }
    }

    /// Number of present hard-disk drives (images, not CD-ROMs) ahead of `field`
    /// in the Boot Priority list order -- its rank for the cascade default.
    fn hdd_boot_rank(&self, field: LauncherField) -> usize {
        BOOTPRI_ROWS
            .iter()
            .take_while(|r| r.field != field)
            .filter(|r| self.boot_field_applies(r.field))
            .count()
    }

    /// The bay a `DfNImage` field belongs to.
    pub fn drive_image_bay(field: LauncherField) -> Option<usize> {
        Some(match field {
            F::Df0Image => 0,
            F::Df1Image => 1,
            F::Df2Image => 2,
            F::Df3Image => 3,
            _ => return None,
        })
    }

    /// The bay a `DfNWriteProtect` field belongs to.
    pub fn drive_protect_bay(field: LauncherField) -> Option<usize> {
        Some(match field {
            F::Df0WriteProtect => 0,
            F::Df1WriteProtect => 1,
            F::Df2WriteProtect => 2,
            F::Df3WriteProtect => 3,
            _ => return None,
        })
    }

    /// The `DfNBridge` tick box for a bay.
    pub fn drive_bridge_field(idx: usize) -> LauncherField {
        match idx {
            0 => F::Df0Bridge,
            1 => F::Df1Bridge,
            2 => F::Df2Bridge,
            _ => F::Df3Bridge,
        }
    }

    /// Whether this bay uses a real drive rather than an image.
    pub fn drive_bridged(&self, idx: usize) -> bool {
        self.df_bridge.get(idx).is_some_and(Option::is_some)
    }

    /// Turn a bay over to a physical drive, or back to images. Switching to a
    /// bridge clears the image: the disk in the drive is the media now.
    ///
    /// Without the feature there is no such thing as a bridged bay -- no tick
    /// box offers one, the config file's keys are ignored -- and this refuses
    /// as well, so no path can leave a bay in a state the build cannot honour.
    #[cfg(feature = "fluxbridge")]
    pub fn set_drive_bridged(&mut self, idx: usize, on: bool) {
        if idx >= self.df_bridge.len() || self.drive_bridged(idx) == on {
            return;
        }
        if on {
            self.df_playlists[idx].clear();
            self.df_bridge[idx] = Some(FluxBridgeConfig::default());
            // Look again now: this is the moment the user expects to be told
            // whether there is anything on the other end.
            self.bridge_status = bridge_status();
            #[cfg(feature = "fluxbridge")]
            {
                self.bridge_ports = sample_bridge_ports();
            }
            // With nothing on the other end, the honest interface is "None";
            // the row is there to change once something is plugged in.
            self.df_bridge_none[idx] = self.bridge_status == BridgeStatus::NoInterface;
            // Wire the bay in, as choosing an image would.
            self.floppy_drives = self.floppy_drives.max(idx as u8 + 1);
        } else {
            self.df_bridge[idx] = None;
            self.df_bridge_none[idx] = false;
        }
    }

    #[cfg(not(feature = "fluxbridge"))]
    pub fn set_drive_bridged(&mut self, _idx: usize, _on: bool) {}

    /// The interface a bridged bay is set to use, for its media row. Naming one
    /// that is not there would suggest the bay is ready to run when it is not,
    /// so what is missing is named instead -- and which of the two it is
    /// decides whether the fix is installing software or plugging in hardware.
    pub fn drive_bridge_label(&self, idx: usize) -> String {
        let Some(cfg) = self.df_bridge.get(idx).and_then(Option::as_ref) else {
            return "(none)".to_string();
        };
        if self.df_bridge_none[idx] {
            return "Not connected".to_string();
        }
        match self.bridge_status {
            BridgeStatus::NoInterface => "Not connected".to_string(),
            BridgeStatus::Attached => cfg.driver.label().to_string(),
        }
    }

    /// What the library can see, for the FluxBridge page's heading.
    pub fn bridge_status(&self) -> BridgeStatus {
        self.bridge_status
    }

    /// Which bay the settings page is editing.
    pub fn bridge_edit_drive(&self) -> usize {
        self.bridge_edit_drive
    }

    /// The controller the SCSI row is showing.
    #[cfg(test)]
    fn scsi_controller_for_test(&self) -> Option<ScsiController> {
        self.scsi_controller
    }

    /// Stand in for the host's storage, so the page can be drawn and tested
    /// without one attached.
    #[cfg(test)]
    pub fn set_host_disks_for_test(&mut self, disks: Vec<HostDiskRow>) {
        self.host_disks = disks;
    }

    /// The disks the Host Disk table is showing.
    pub fn host_disks(&self) -> &[HostDiskRow] {
        &self.host_disks
    }

    /// First row shown in the table.
    pub fn host_disk_scroll(&self) -> usize {
        self.host_disk_scroll
    }

    pub fn host_disk_scroll_rate(&mut self) -> &mut ScrollRate {
        &mut self.host_disk_scroll_rate
    }

    /// Move the window over the list, stopping at either end. `visible` is
    /// how many rows the box shows, which is the caller's geometry to know.
    pub fn scroll_host_disks(&mut self, delta: isize, visible: usize) {
        let last_start = self.host_disks.len().saturating_sub(visible);
        let at = self.host_disk_scroll as isize + delta;
        self.host_disk_scroll = at.clamp(0, last_start as isize) as usize;
    }

    /// Look at the host's storage again. Called when the page opens and from
    /// its Refresh button, so a card pushed in mid-session appears without the
    /// launcher polling for it.
    pub fn refresh_host_disks(&mut self) {
        // Looking again should not undo what was chosen: a disk still present
        // keeps the read-only and attachment it was given.
        let previous = std::mem::take(&mut self.host_disks);
        self.host_disks = sample_host_disks();
        self.reconcile_host_disk_rows(&previous);
    }

    fn reconcile_host_disk_rows(&mut self, previous: &[HostDiskRow]) {
        let selected_before: Vec<(&str, Option<&str>)> = previous
            .iter()
            .filter(|row| self.host_disk_selected.contains(&row.id))
            .map(|row| (row.id.as_str(), row.fingerprint.as_deref()))
            .collect();
        // A shorter list must not leave the window past its end.
        self.host_disk_scroll = self.host_disk_scroll.min(self.host_disks.len());
        for row in &mut self.host_disks {
            if let Some(old) = previous.iter().find(|old| {
                match (row.fingerprint.as_deref(), old.fingerprint.as_deref()) {
                    (Some(new), Some(old)) => new == old,
                    _ => old.id == row.id,
                }
            }) {
                row.writable = old.writable;
                row.attach = old.attach;
            }
        }
        // A disk that has gone (unplugged between looks) cannot stay ticked.
        self.host_disk_selected = self
            .host_disks
            .iter()
            .filter(|row| {
                selected_before.iter().any(|(old_id, old_fingerprint)| {
                    match (row.fingerprint.as_deref(), *old_fingerprint) {
                        (Some(new), Some(old)) => new == old,
                        _ => row.id == *old_id,
                    }
                })
            })
            .map(|row| row.id.clone())
            .collect();
        // The page shows the machine as it stands: a disk already given to it
        // is ticked, sitting where it was put.
        for attached in &mut self.host_disks_attached {
            if let Some(row) = self.host_disks.iter_mut().find(|row| {
                attached
                    .fingerprint
                    .as_ref()
                    .zip(row.fingerprint.as_ref())
                    .is_some_and(|(saved, current)| saved == current)
                    || ((attached.fingerprint.is_none() || row.fingerprint.is_none())
                        && row.id == attached.device)
            }) {
                attached.device = row.id.clone();
                attached.fingerprint = row.fingerprint.clone();
                row.attach = Some(attached.attach);
                row.writable = attached.writable;
                if !self.host_disk_selected.contains(&attached.device) {
                    self.host_disk_selected.push(attached.device.clone());
                }
            }
        }
    }

    /// The real disks currently given to the machine.
    pub fn host_disks_attached(&self) -> &[crate::config::HostDiskConfig] {
        &self.host_disks_attached
    }

    /// What is attached at one point, if anything.
    pub fn host_disk_at(
        &self,
        attach: crate::config::HostDiskAttach,
    ) -> Option<&crate::config::HostDiskConfig> {
        self.host_disks_attached.iter().find(|d| d.attach == attach)
    }

    /// Whether a disk is currently given to the machine.
    pub fn host_disk_is_attached(&self, device: &str) -> bool {
        self.host_disks_attached.iter().any(|d| d.device == device)
    }

    /// Whether a disk is ticked.
    pub fn host_disk_is_selected(&self, device: &str) -> bool {
        self.host_disk_selected.iter().any(|d| d == device)
    }

    /// The ticked disks, in the order they were ticked.
    pub fn host_disks_selected(&self) -> &[String] {
        &self.host_disk_selected
    }

    /// Whether the machine has the port an attachment point needs.
    fn attach_is_fitted(&self, attach: crate::config::HostDiskAttach) -> bool {
        use crate::config::HostDiskAttach as A;
        match attach {
            A::IdeMaster | A::IdeSlave => self.has_ide(),
            A::Scsi(_) => self.has_scsi_controller(),
            A::LideMaster(ch) | A::LideSlave(ch) => self
                .lide_board
                .is_some_and(|b| usize::from(ch) < b.channels()),
        }
    }

    /// The first attachment point nothing has claimed, preferring the ones
    /// the machine can actually use so a tick lands somewhere useful.
    fn free_host_disk_attach(&self) -> Option<crate::config::HostDiskAttach> {
        let claimed: Vec<_> = self
            .host_disks
            .iter()
            .filter(|row| self.host_disk_is_selected(&row.id))
            .filter_map(|row| row.attach)
            .collect();
        crate::config::HostDiskAttach::all()
            .into_iter()
            .filter(|a| !claimed.contains(a))
            .find(|a| self.attach_is_fitted(*a))
    }

    /// Give back any real disk whose attachment point the machine no longer
    /// has. Taking the SCSI controller out, or moving to a model with no IDE
    /// port, leaves a disk configured somewhere nothing can reach it; the
    /// disk quietly goes rather than being carried to Run and refused there.
    fn drop_unreachable_host_disks(&mut self) {
        let gone: Vec<_> = self
            .host_disks_attached
            .iter()
            .filter(|disk| !self.attach_is_fitted(disk.attach))
            .map(|disk| (disk.device.clone(), disk.attach))
            .collect();
        for (device, attach) in gone {
            log::info!(
                "host disk: {device} taken off {} -- the machine no longer has it",
                attach.label()
            );
            self.host_disks_attached.retain(|d| d.attach != attach);
            self.host_disk_selected.retain(|id| *id != device);
            if let Some(row) = self.host_disks.iter_mut().find(|d| d.id == device) {
                row.attach = None;
            }
            // This is an unmount like any other, so the hold goes with it.
            // Leaving it would keep the disk from the host with nothing left
            // on screen to release it -- no row, no tick, no Unmount.
            #[cfg(not(target_arch = "wasm32"))]
            crate::blockdev::release_device(&device);
        }
    }

    /// Why the last tick did not take, if it did not.
    pub fn host_disk_warning(&self) -> Option<&str> {
        self.host_disk_warning.as_deref()
    }

    /// Tick or untick one disk.
    ///
    /// Ticking is what gives the disk a place: the first attachment point
    /// still free, IDE Master before anything else. Unticking takes the
    /// place away again, and the cell reads blank -- an unticked disk is
    /// going nowhere, and a place named on one would be a claim nothing is
    /// making.
    pub fn select_host_disk(&mut self, index: usize) {
        let Some(id) = self.host_disks.get(index).map(|d| d.id.clone()) else {
            return;
        };
        self.host_disk_warning = None;
        if self.host_disk_is_selected(&id) {
            self.host_disk_selected.retain(|d| *d != id);
            if let Some(row) = self.host_disks.get_mut(index) {
                row.attach = None;
            }
            return;
        }
        // A disk the machine already has goes back to the place it holds:
        // ticking it again is recognising the attachment, not asking for a
        // second one somewhere new.
        if let Some(attached) = self
            .host_disks_attached
            .iter()
            .find(|d| d.device == id)
            .map(|d| d.attach)
        {
            if let Some(row) = self.host_disks.get_mut(index) {
                row.attach = Some(attached);
            }
            self.host_disk_selected.push(id);
            return;
        }
        match self.free_host_disk_attach() {
            Some(free) => {
                if let Some(row) = self.host_disks.get_mut(index) {
                    row.attach = Some(free);
                }
                self.host_disk_selected.push(id);
            }
            // Nowhere to put this disk, so the tick does not take and the
            // reason is shown: silently ticking a disk that could not be
            // attached would be a lie found out only at Mount. Which reason
            // depends on why -- a machine with no port at all is a different
            // problem from one whose ports are full.
            None => {
                let any_port = crate::config::HostDiskAttach::all()
                    .into_iter()
                    .any(|a| self.attach_is_fitted(a));
                self.host_disk_warning = Some(if any_port {
                    "Every attachment point is already in use".to_string()
                } else {
                    crate::config::HostDiskAttach::no_port_requirement().to_string()
                });
            }
        }
    }

    /// What to call a disk that is attached: the volume the host reported for
    /// it, or its identifier when it is not attached to this computer now --
    /// a configuration outlives the card reader it was written at.
    pub fn host_disk_volume(&self, device: &str) -> String {
        self.host_disks
            .iter()
            .find(|d| d.id == device)
            .map(|d| d.volume.clone())
            .unwrap_or_else(|| device.to_string())
    }

    /// How a slot holding a real disk reads: the device the host calls it,
    /// and the volume on it. A configuration can outlive the disk it names,
    /// so one that is not there says so rather than reading as normal.
    pub fn host_disk_label(&self, device: &str) -> String {
        match self.host_disks.iter().find(|d| d.id == device) {
            // The volume alone: the row's own label already says which drive
            // this is, and Unmount beside it already says it is a real disk.
            // (On Windows the "volume" is the model string the bridge
            // reports, `Generic MassStorageClass USB Device` and the like --
            // there is nothing shorter that still names the hardware.)
            Some(disk) => disk.volume.clone(),
            // Nothing has been sampled yet, so nothing is known: the Host Disk
            // page looks when it opens, and a launcher that has not been there
            // has not asked. A disk sitting in the reader called "not
            // connected" is worse than one described only by its name.
            None if self.host_disks.is_empty() => device.to_string(),
            None => format!("{device} (not connected)"),
        }
    }

    /// Give every ticked disk to the machine.
    ///
    /// A slot holds one thing, so whatever was there -- another disk, or an
    /// image -- makes way. Disks whose attachment point the machine does not
    /// have are left alone and reported: configuring a disk somewhere it can
    /// never be reached is worse than saying so.
    pub fn mount_host_disks(&mut self) -> Result<Vec<crate::config::HostDiskConfig>, String> {
        if self.host_disk_selected.is_empty() {
            return Err("Select a disk to attach it to the machine".to_string());
        }
        // A ticked disk always has a place -- ticking is what assigns one --
        // so a tick without one is a bug worth failing loudly on, not a case.
        let chosen: Vec<(HostDiskRow, crate::config::HostDiskAttach)> = self
            .host_disks
            .iter()
            .filter(|row| self.host_disk_is_selected(&row.id))
            .map(|row| {
                let attach = row.attach.expect("a ticked disk has an attachment point");
                (row.clone(), attach)
            })
            .collect();
        if let Some((_, bad)) = chosen
            .iter()
            .find(|(_, attach)| !self.attach_is_fitted(*attach))
        {
            return Err(bad.requirement().to_string());
        }

        let mut attached = Vec::new();
        for (row, attach) in chosen {
            let entry = crate::config::HostDiskConfig {
                device: row.id.clone(),
                fingerprint: row.fingerprint.clone(),
                identity_confirmed: true,
                attach,
                writable: row.writable,
            };
            self.host_disks_attached
                .retain(|d| d.attach != entry.attach);
            self.clear_slot(entry.attach);
            self.host_disks_attached.push(entry.clone());
            attached.push(entry);
        }
        Ok(attached)
    }

    /// Empty whatever image was in a slot a disk is taking.
    fn clear_slot(&mut self, attach: crate::config::HostDiskAttach) {
        match attach {
            crate::config::HostDiskAttach::IdeMaster => {
                self.ide_master = None;
                self.ide_master_name = None;
            }
            crate::config::HostDiskAttach::IdeSlave => {
                self.ide_slave = None;
                self.ide_slave_name = None;
            }
            crate::config::HostDiskAttach::LideMaster(ch)
            | crate::config::HostDiskAttach::LideSlave(ch) => {
                let idx = usize::from(ch) * 2
                    + usize::from(matches!(
                        attach,
                        crate::config::HostDiskAttach::LideSlave(_)
                    ));
                if let Some(slot) = self.lide_drives.get_mut(idx) {
                    *slot = None;
                }
                if let Some(name) = self.lide_drive_names.get_mut(idx) {
                    *name = None;
                }
                // `[lide] drives` is a positional list -- a hole cannot be
                // represented -- so a host disk taking over this slot must
                // cascade-clear every later slot too, exactly like
                // `clear_path` does for the image case, or `to_raw`'s
                // `map_while` would silently stop emitting at this slot and
                // drop any image still sitting in a later one.
                for slot in idx + 1..self.lide_drives.len() {
                    self.lide_drives[slot] = None;
                    self.lide_drive_names[slot] = None;
                    self.lide_drive_bootpri[slot] = None;
                    self.lide_drive_boot_off[slot] = false;
                }
            }
            crate::config::HostDiskAttach::Scsi(unit) => {
                if let Some(slot) = self.scsi_units.get_mut(usize::from(unit)) {
                    *slot = None;
                }
                if let Some(name) = self.scsi_unit_names.get_mut(usize::from(unit)) {
                    *name = None;
                }
            }
        }
    }

    /// Take a disk back off the machine and hand it to the host.
    pub fn unmount_host_disk(&mut self, attach: crate::config::HostDiskAttach) -> Option<String> {
        let at = self
            .host_disks_attached
            .iter()
            .position(|d| d.attach == attach)?;
        let device = self.host_disks_attached.remove(at).device;
        // The Host Disk page mirrors what is attached, so taking the disk
        // off the machine unticks it there too -- a tick surviving the
        // unmount would read as still attached.
        self.host_disk_selected.retain(|id| *id != device);
        if let Some(row) = self.host_disks.iter_mut().find(|d| d.id == device) {
            row.attach = None;
        }
        Some(device)
    }

    /// Take every ticked disk that the machine has back off it, and say
    /// which went. The Host Disk page's Unmount: the ticks say which disks
    /// are meant, exactly as they do for Mount.
    pub fn unmount_selected_host_disks(&mut self) -> Vec<String> {
        let attached: Vec<crate::config::HostDiskAttach> = self
            .host_disks_attached
            .iter()
            .filter(|d| self.host_disk_is_selected(&d.device))
            .map(|d| d.attach)
            .collect();
        attached
            .into_iter()
            .filter_map(|attach| self.unmount_host_disk(attach))
            .collect()
    }

    /// Flip whether the guest may write to one disk.
    pub fn toggle_host_disk_writable(&mut self, index: usize) {
        if let Some(row) = self.host_disks.get_mut(index) {
            row.writable = !row.writable;
        }
    }

    /// Step one disk's attachment point, skipping the ones another ticked
    /// disk has already claimed so the cycle only offers places it can go.
    pub fn cycle_host_disk_attach(&mut self, index: usize, forward: bool) {
        let Some(row) = self.host_disks.get(index) else {
            return;
        };
        // Only a ticked disk has a place to step: the cell is blank
        // otherwise, and clicking blank is not a request anybody made.
        let (Some(current), true) = (row.attach, self.host_disk_is_selected(&row.id)) else {
            return;
        };
        let id = row.id.clone();
        let claimed: Vec<_> = self
            .host_disks
            .iter()
            .filter(|r| r.id != id && self.host_disk_is_selected(&r.id))
            .filter_map(|r| r.attach)
            .collect();
        let options: Vec<_> = crate::config::HostDiskAttach::all()
            .into_iter()
            .filter(|a| *a == current || !claimed.contains(a))
            .collect();
        let at = options.iter().position(|a| *a == current).unwrap_or(0);
        let next = if forward {
            (at + 1) % options.len()
        } else {
            (at + options.len() - 1) % options.len()
        };
        if let Some(row) = self.host_disks.get_mut(index) {
            row.attach = Some(options[next]);
        }
    }

    /// The attachment point a Storage-page row stands for, for the rows that
    /// can hold a real disk.
    pub fn host_disk_attach_of(field: F) -> Option<crate::config::HostDiskAttach> {
        match field {
            F::IdeMaster => Some(crate::config::HostDiskAttach::IdeMaster),
            F::IdeSlave => Some(crate::config::HostDiskAttach::IdeSlave),
            F::LideDrive0 => Some(crate::config::HostDiskAttach::LideMaster(0)),
            F::LideDrive1 => Some(crate::config::HostDiskAttach::LideSlave(0)),
            F::LideDrive2 => Some(crate::config::HostDiskAttach::LideMaster(1)),
            F::LideDrive3 => Some(crate::config::HostDiskAttach::LideSlave(1)),
            F::ScsiUnit0 => Some(crate::config::HostDiskAttach::Scsi(0)),
            F::ScsiUnit1 => Some(crate::config::HostDiskAttach::Scsi(1)),
            F::ScsiUnit2 => Some(crate::config::HostDiskAttach::Scsi(2)),
            F::ScsiUnit3 => Some(crate::config::HostDiskAttach::Scsi(3)),
            F::ScsiUnit4 => Some(crate::config::HostDiskAttach::Scsi(4)),
            F::ScsiUnit5 => Some(crate::config::HostDiskAttach::Scsi(5)),
            F::ScsiUnit6 => Some(crate::config::HostDiskAttach::Scsi(6)),
            _ => None,
        }
    }

    /// The real disk sitting on a Storage-page row, if one is.
    pub fn host_disk_on_row(&self, field: F) -> Option<&crate::config::HostDiskConfig> {
        self.host_disk_at(Self::host_disk_attach_of(field)?)
    }

    pub fn set_bridge_edit_drive(&mut self, idx: usize) {
        self.bridge_edit_drive = idx.min(3);
    }

    /// Whether the bridge page is editing a bay with an interface actually
    /// selected and attached. Only then does that interface's own set of
    /// capabilities decide anything: with none there is nothing to have an
    /// opinion, and the rows it would shape have nothing to show.
    pub fn bridge_interface_selected(&self) -> bool {
        self.bridge_edit().is_some()
            && !self.df_bridge_none[self.bridge_edit_drive]
            && self.bridge_status == BridgeStatus::Attached
    }

    /// The settings being shown on the FluxBridge page.
    fn bridge_edit(&self) -> Option<&FluxBridgeConfig> {
        self.df_bridge[self.bridge_edit_drive].as_ref()
    }

    fn bridge_edit_mut(&mut self) -> Option<&mut FluxBridgeConfig> {
        self.df_bridge[self.bridge_edit_drive].as_mut()
    }

    /// Whether the interface this page is editing honours one of the library's
    /// optional settings, as that driver itself reports.
    ///
    /// Each driver advertises what it supports, and they differ: a Greaseweazle
    /// takes a drive-select cable, a DrawBridge answers to a different set.
    /// Offering a switch the hardware ignores is how a user ends up believing
    /// they changed something, so the ones it does not honour are greyed with
    /// the interface's name against them.
    #[cfg(feature = "fluxbridge")]
    fn bridge_driver_supports(&self, option: u32) -> bool {
        let Some(cfg) = self.bridge_edit() else {
            return false;
        };
        // A driver this build does not carry (a config written for another
        // build) cannot be asked, and leaving its rows live is better than
        // greying out a page the user cannot then fix.
        crate::fluxbridge::driver_named(cfg.driver.match_token())
            .is_none_or(|driver| driver.supports(option))
    }

    /// Serial ports to offer, "Automatic" first -- the default, and what
    /// every current interface supports. The library's own scan leads, then
    /// every other serial device the host has, so an interface on a chip the
    /// scan's naming misses -- an Arduino clone mounting as
    /// `tty.wchusbserial*` on macOS, say -- can still be picked by hand. The
    /// names are the host's own: `/dev/cu.usbmodem101` on macOS,
    /// `/dev/ttyACM0` on Linux, `COM3` on Windows.
    fn bridge_port_options(&self) -> Vec<Option<String>> {
        #[cfg(feature = "fluxbridge")]
        {
            self.bridge_ports.clone()
        }
        #[cfg(not(feature = "fluxbridge"))]
        {
            vec![None]
        }
    }

    pub fn zorro_boards(&self) -> &[ZorroBoardSetup] {
        &self.zorro_boards
    }

    pub fn add_zorro(&mut self, path: PathBuf) {
        self.zorro_boards.push(ZorroBoardSetup::load(path));
    }

    pub fn remove_zorro(&mut self, idx: usize) {
        if idx < self.zorro_boards.len() {
            self.zorro_boards.remove(idx);
        }
    }

    /// Step an enum/int option on a board.
    pub fn zorro_option_cycle(&mut self, board: usize, opt: usize, forward: bool) {
        if let Some(b) = self.zorro_boards.get_mut(board) {
            b.cycle(opt, forward);
        }
    }

    /// Flip a bool option on a board.
    pub fn zorro_option_toggle(&mut self, board: usize, opt: usize) {
        if let Some(b) = self.zorro_boards.get_mut(board) {
            b.toggle(opt);
        }
    }

    /// Set a board option's value (a file path, or typed text).
    pub fn zorro_option_set(&mut self, board: usize, opt: usize, value: String) {
        if let Some(b) = self.zorro_boards.get_mut(board) {
            b.set(opt, value);
        }
    }

    /// Revert a board option to its manifest default.
    pub fn zorro_option_clear(&mut self, board: usize, opt: usize) {
        if let Some(b) = self.zorro_boards.get_mut(board) {
            b.clear(opt);
        }
    }
}

/// What a status line is saying, which is also how it is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// It worked.
    Ok,
    /// It is still happening. Shown while a long piece of host work runs,
    /// so the panel says what it is waiting for rather than going quiet.
    Busy,
    /// It did not work, or will not.
    Error,
}

/// A short status/error line shown along the bottom of the configuration panel.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub kind: StatusKind,
}

impl StatusMessage {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Ok,
        }
    }

    pub fn busy(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Busy,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusKind::Error,
        }
    }
}

/// A text field that has keyboard focus in the configuration panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    /// A Zorro plugin board's string option (board index, option index).
    BoardOption { board: usize, opt: usize },
    /// A hard-drive volume-name override.
    DriveName(LauncherField),
    /// A hard-disk boot priority typed as a number (the Boot Priority page).
    DriveBootpri(LauncherField),
    /// A word typed on a Create Image page: a volume name or a device name.
    NewImageText(LauncherField),
    /// A `host:port` typed into the Serial section's Connect or Listen box.
    SerialAddr(LauncherField),
    /// The fixed 16-bit RAM power-on word on the Memory page.
    RamPattern,
}

/// Where typing goes in a text field, as a character index into it.
///
/// One of these sits beside every editable line in the launcher -- the value
/// boxes on the configuration pages and the boxes in the WHDLoad dialogs --
/// so all of them insert, delete and step alike, and all of them draw the
/// same block over the character the caret is on. Without it a box can only
/// be typed at the end, which is no way to correct a long path or amend
/// metadata that is nearly right.
///
/// Characters, not bytes: the fields hold whatever a name or a title is
/// spelt with, and stepping half way into one would split it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caret(usize);

impl Caret {
    /// Past the last character, which is where opening a box for editing
    /// puts it: what is already in there is usually a thing to add to.
    pub fn end_of(text: &str) -> Self {
        Self::at_end_of_len(text.chars().count())
    }

    /// The same for a line whose text is not to be handed out -- a masked
    /// password, which is counted rather than read.
    pub fn at_end_of_len(len: usize) -> Self {
        Self(len)
    }

    /// Which character the caret is on, for drawing the block.
    pub fn at(self) -> usize {
        self.0
    }

    /// Pull the caret back inside a line it may now be off the end of --
    /// the focus moved to a shorter field, or the value was replaced.
    fn clamp(&mut self, text: &str) {
        self.0 = self.0.min(text.chars().count());
    }

    pub fn left(&mut self) {
        self.0 = self.0.saturating_sub(1);
    }

    pub fn right(&mut self, text: &str) {
        self.right_of(text.chars().count());
    }

    /// Step right within a line of `len` characters.
    pub fn right_of(&mut self, len: usize) {
        self.0 = (self.0 + 1).min(len);
    }

    pub fn home(&mut self) {
        self.0 = 0;
    }

    pub fn end(&mut self, text: &str) {
        self.0 = text.chars().count();
    }

    /// The same for a counted line.
    pub fn end_of_len(&mut self, len: usize) {
        self.0 = len;
    }

    /// The byte offset the caret sits at, which is the end of the string
    /// when it is past the last character.
    fn byte_in(self, text: &str) -> usize {
        text.char_indices()
            .nth(self.0)
            .map_or(text.len(), |(at, _)| at)
    }

    /// Insert at the caret and step over what was typed.
    pub fn insert(&mut self, text: &mut String, c: char) {
        // Against a line that was changed without the caret being told --
        // a field reset, a value replaced wholesale -- rather than reaching
        // off the end of it.
        self.clamp(text);
        let at = self.byte_in(text);
        text.insert(at, c);
        self.0 += 1;
    }

    /// Delete the character before the caret, as Backspace does. False when
    /// there is nothing behind it.
    pub fn backspace(&mut self, text: &mut String) -> bool {
        self.clamp(text);
        if self.0 == 0 {
            return false;
        }
        self.left();
        let at = self.byte_in(text);
        text.remove(at);
        true
    }

    /// Delete the character the caret is on, as Delete does. False at the
    /// end of the line, where it is on nothing.
    pub fn delete(&mut self, text: &mut String) -> bool {
        let at = self.byte_in(text);
        if at >= text.len() {
            return false;
        }
        text.remove(at);
        true
    }
}

/// How fast a held scroll runs.
///
/// A list can be a few hundred rows and a keypress is one of them, so
/// reaching the far end a row at a time is a lot of pressing. Holding it
/// instead runs through five speeds, a second at each, and the last one
/// crosses a library in about a second. Starting slow is the point: most
/// scrolling is a few rows, and a control that leapt away on the first
/// repeat would overshoot every time.
///
/// Every list in the launcher scrolls through one of these -- the WHDLoad
/// games and favourites, the host disks -- and both ways of driving them,
/// a held arrow key and a held scroll-arrow button, go through the same
/// one, so they run at the same speed as each other.
///
/// Letting go and pressing again starts from the bottom: the speed is
/// measured from when the run of scrolling began, and a gap longer than a
/// repeat begins a new run.
#[derive(Debug, Clone, Default)]
pub struct ScrollRate {
    /// When the last movement was, to tell a continued scroll from a fresh
    /// one.
    last: Option<std::time::Instant>,
    /// When the run of scrolling this movement belongs to began, which is
    /// what the speed is worked out from.
    started: Option<std::time::Instant>,
}

impl ScrollRate {
    /// Rows a step, a stage a second. The last is what a long list needs
    /// and the first is what a short one does.
    const STAGES: [usize; 5] = [1, 3, 7, 14, 24];
    /// How long each stage lasts before the next takes over.
    const STAGE: std::time::Duration = std::time::Duration::from_secs(1);
    /// The gap after which a movement starts a new run rather than
    /// continuing one. Longer than the repeat of anything being held --
    /// the button repeat here, or a host keyboard's -- and shorter than a
    /// pause that means to stop.
    const CONTINUES_WITHIN: std::time::Duration = std::time::Duration::from_millis(250);

    /// How many rows this step should move, given when it arrived.
    pub fn rows_for_step(&mut self, now: std::time::Instant) -> usize {
        let continued = self
            .last
            .is_some_and(|last| now.duration_since(last) <= Self::CONTINUES_WITHIN);
        let started = match continued {
            true => self.started.unwrap_or(now),
            false => now,
        };
        self.started = Some(started);
        self.last = Some(now);
        let stage = now.duration_since(started).as_millis() / Self::STAGE.as_millis();
        Self::STAGES[(stage as usize).min(Self::STAGES.len() - 1)]
    }

    /// Back to the first stage, for a press that is deliberately a new one
    /// rather than the continuation of an old one.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The buckets the A-Z shortcut row offers, in the order it draws them.
///
/// Digits first, then everything that starts with neither a digit nor a
/// letter, then the alphabet. A game is filed by the first character of the
/// name the list shows for it, so what you click matches what you read.
#[cfg(feature = "game-library")]
pub const AZ_BUCKETS: usize = 28;

/// The label on bucket `at`.
#[cfg(feature = "game-library")]
pub fn az_label(at: usize) -> &'static str {
    const LETTERS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    match at {
        0 => "0-9",
        1 => "#",
        _ => LETTERS.get(at - 2).copied().unwrap_or("#"),
    }
}

/// Which bucket a title belongs to.
///
/// The initial is folded to ASCII first, the same way the panel folds it
/// to draw it: "Élite" is drawn as "Elite" and sorted among the E's, so it
/// answers to E rather than sitting under `#` where nobody would look for
/// it.
#[cfg(feature = "game-library")]
pub fn az_bucket_of(title: &str) -> usize {
    let first = title.chars().next().map(|c| match c.is_ascii() {
        true => c,
        false => crate::video::font::fold(c).chars().next().unwrap_or(c),
    });
    match first {
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => 2 + (c.to_ascii_uppercase() as usize - 'A' as usize),
        _ => 1,
    }
}

/// Which way a caret is being stepped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaretMove {
    Left,
    Right,
    Home,
    End,
}

/// The most an address box accepts: a DNS name is up to 253 characters
/// (254 with a trailing root dot), and ":65535" is six more. Anything
/// longer cannot be a host:port, so the box stops there.
const SERIAL_ADDR_MAX: usize = 260;

/// Why a typed serial address is not the `host:port` the TCP modes need,
/// or `None` when it is one.
///
/// Nothing here resolves a name or opens a socket: that happens at Run, and
/// a host that is merely down should not stop the address being typed. What
/// it does catch is the shape, so a missing or unreadable port is refused
/// while the box still has the focus to fix it in.
fn serial_addr_error(addr: &str) -> Option<String> {
    let Some((host, port)) = addr.rsplit_once(':') else {
        return Some(format!("\"{addr}\" has no port: type host:port"));
    };
    // An IPv6 literal is full of colons, so it is written in brackets and
    // only the colon after the closing one separates the port.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Some(format!("\"{addr}\" has no host: type host:port"));
    }
    if port.parse::<u16>().is_err() {
        return Some(format!("\"{port}\" is not a port number (0-65535)"));
    }
    None
}

/// The largest size the box accepts, in whichever unit is showing.
/// The largest number the size box accepts: four digits, so 9999 GB is the
/// most that can be asked for. Past 2 TiB only an unpartitioned,
/// unformatted drive is possible -- see [`crate::diskimage::MAX_RDB_BYTES`].
const NEW_HARD_SIZE_MAX: u32 = 9999;

/// The unit the hard-drive size is typed in. Clicking it swaps to the
/// other, keeping the number: 8 MB becomes 8 GB.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SizeUnit {
    #[default]
    Mb,
    Gb,
}

impl SizeUnit {
    pub fn label(self) -> &'static str {
        match self {
            SizeUnit::Mb => "MB",
            SizeUnit::Gb => "GB",
        }
    }

    fn bytes(self) -> u64 {
        match self {
            SizeUnit::Mb => 1 << 20,
            SizeUnit::Gb => 1 << 30,
        }
    }

    fn flipped(self) -> SizeUnit {
        match self {
            SizeUnit::Mb => SizeUnit::Gb,
            SizeUnit::Gb => SizeUnit::Mb,
        }
    }
}

/// What the next image will be made of.
///
/// Deliberately not part of [`MachineSetup`]: nothing here describes the
/// machine, so none of it belongs in a configuration file. It lives for as
/// long as the launcher is open and is thrown away with it.
#[derive(Debug, Clone)]
pub struct ImageWorkshop {
    pub density: crate::diskimage::Density,
    pub container: crate::diskimage::Container,
    /// `None` is an unformatted disk, for the guest to format itself.
    pub floppy_fs: Option<crate::diskimage::FileSystem>,
    pub floppy_label: String,
    pub floppy_bootable: bool,
    /// The size as typed, in [`ImageWorkshop::size_unit`].
    pub size: u32,
    pub size_unit: SizeUnit,
    /// Whether the geometry is the size's own or one set by hand.
    pub geometry_custom: bool,
    /// The geometry set by hand, once it has been. Kept while Auto is
    /// showing so going back to Custom finds it where it was left.
    pub custom_geometry: crate::diskimage::Geometry,
    pub partitioning: crate::diskimage::Partitioning,
    pub device: String,
    pub hard_fs: Option<crate::diskimage::FileSystem>,
    pub hard_label: String,
    pub hard_bootable: bool,
    pub boot_pri: i8,
    /// Blocks kept clear at the front of the partition.
    pub reserved: u32,
    /// What the drive says it is, per field, once that field has been
    /// told. A field left `None` -- the usual case -- keeps naming itself,
    /// so the Type follows the Size box instead of going stale behind it,
    /// and typing a Drive does not freeze the Type along with it.
    pub vendor: Option<String>,
    pub product: Option<String>,
    pub revision: Option<String>,
    pub read_only: bool,
    /// Leave the file's unwritten blocks as holes on the host. On by
    /// default: a sparse image is instant to make and costs only what it
    /// is actually used for.
    pub sparse: bool,
}

impl Default for ImageWorkshop {
    fn default() -> Self {
        Self {
            density: crate::diskimage::Density::Dd,
            container: crate::diskimage::Container::Adf,
            // A plain OFS floppy is the one that mounts on every Amiga
            // ever made, so it is where the page starts.
            floppy_fs: Some(crate::diskimage::FileSystem::OFS),
            floppy_label: "Empty".to_string(),
            floppy_bootable: false,
            size: 64,
            size_unit: SizeUnit::Mb,
            geometry_custom: false,
            custom_geometry: crate::diskimage::Geometry::for_size(64 << 20),
            partitioning: crate::diskimage::Partitioning::Rdb,
            device: "DH0".to_string(),
            hard_fs: Some(crate::diskimage::FileSystem::FFS),
            hard_label: "Work".to_string(),
            hard_bootable: true,
            boot_pri: 0,
            reserved: crate::diskimage::RESERVED_BLOCKS,
            vendor: None,
            product: None,
            revision: None,
            read_only: false,
            sparse: true,
        }
    }
}

impl ImageWorkshop {
    pub fn bytes(&self) -> u64 {
        u64::from(self.size.max(1)) * self.size_unit.bytes()
    }

    /// Swap MB and GB, keeping the number: 8 MB becomes 8 GB.
    pub fn flip_size_unit(&mut self) {
        self.size_unit = self.size_unit.flipped();
    }

    /// The geometry the image will carry: the one set by hand, or the one
    /// the size implies.
    pub fn effective_geometry(&self) -> crate::diskimage::Geometry {
        if self.geometry_custom {
            self.custom_geometry
        } else {
            crate::diskimage::Geometry::for_size(self.bytes())
        }
    }

    /// Fill the custom geometry in from the size, and put the drive's
    /// identity back to naming itself. This is what the editor's Auto
    /// button does: everything on the page returns to what Copperline
    /// would have chosen.
    pub fn geometry_from_size(&mut self) {
        self.custom_geometry = crate::diskimage::Geometry::for_size(self.bytes());
        self.vendor = None;
        self.product = None;
        self.revision = None;
    }

    /// What the drive will say it is: each field as typed, or as the drive
    /// names itself. Resolved on the way out rather than stored, so a field
    /// nobody has touched still follows the size.
    pub fn identity(&self) -> crate::harddrive::RdbIdentity {
        let named = crate::harddrive::default_rdb_identity(self.effective_geometry().bytes());
        crate::harddrive::RdbIdentity {
            vendor: self.vendor.clone().unwrap_or(named.vendor),
            product: self.product.clone().unwrap_or(named.product),
            revision: self.revision.clone().unwrap_or(named.revision),
        }
    }

    /// Take one identity field as typed, leaving the others naming
    /// themselves: editing the Drive does not freeze the Type at whatever
    /// size happened to be showing.
    pub fn set_identity_field(&mut self, field: LauncherField, text: String) {
        match field {
            F::NewGeomVendor => self.vendor = Some(text),
            F::NewGeomProduct => self.product = Some(text),
            F::NewGeomRevision => self.revision = Some(text),
            _ => {}
        }
    }

    pub fn floppy_spec(&self) -> crate::diskimage::FloppySpec {
        crate::diskimage::FloppySpec {
            density: self.density,
            container: self.container,
            filesystem: self.floppy_fs,
            bootable: self.floppy_bootable && self.floppy_fs.is_some(),
            label: self.floppy_label.clone(),
        }
    }

    pub fn hard_spec(&self) -> crate::diskimage::HardSpec {
        crate::diskimage::HardSpec {
            bytes: self.bytes(),
            geometry: self.geometry_custom.then_some(self.custom_geometry),
            partitioning: self.partitioning,
            filesystem: self.hard_fs,
            device: self.device.clone(),
            label: self.hard_label.clone(),
            // The boot flag and its rank live in the partition entry, so
            // without a partition table there is nothing to carry them:
            // the spec says what will happen, not what the page shows.
            bootable: self.hard_bootable
                && self.partitioning == crate::diskimage::Partitioning::Rdb,
            boot_pri: self.boot_pri,
            reserved: self.reserved,
            identity: Some(self.identity()),
            read_only: self.read_only,
            sparse: self.sparse,
        }
    }

    /// A file name to offer in the save dialog, from what is being made.
    pub fn suggested_name(&self, floppy: bool) -> String {
        let stem = |s: &str| {
            let cleaned: String = s
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>();
            if cleaned.is_empty() {
                "image".to_string()
            } else {
                cleaned
            }
        };
        if floppy {
            format!("{}.adf", stem(&self.floppy_label))
        } else {
            format!("{}.hdf", stem(&self.hard_label))
        }
    }
}

/// How many characters a workshop text field takes, or `None` when it is
/// not one of the fixed-width ones.
///
/// The identity boxes are exactly as wide as the Rigid Disk Block's fields,
/// because a longer string would not be cut so much as spill into the next
/// field -- which is the bug that made a drive called `Copperli` of type
/// `ne`. Stopping the typing at the width is the honest place to stop it.
fn workshop_text_limit(field: LauncherField) -> Option<usize> {
    let widths = crate::harddrive::RDB_IDENTITY_WIDTHS;
    match field {
        F::NewGeomVendor => Some(widths[0]),
        F::NewGeomProduct => Some(widths[1]),
        F::NewGeomRevision => Some(widths[2]),
        _ => None,
    }
}

/// How many characters a workshop number field takes, or `None` when the
/// field is a word rather than a number.
fn workshop_digit_limit(field: LauncherField) -> Option<usize> {
    match field {
        // 9999 is the largest size the box accepts.
        F::NewHardSize => Some(4),
        // -128..=127.
        F::NewHardBootPri => Some(4),
        F::NewGeomCylinders | F::NewGeomSurfaces | F::NewGeomSectors => Some(5),
        F::NewGeomReserved => Some(3),
        _ => None,
    }
}

/// Nudge a geometry figure by one, keeping it inside `floor..=ceiling`.
fn step_u32(value: u32, forward: bool, floor: u32, ceiling: u32) -> u32 {
    if forward {
        value.saturating_add(1).min(ceiling)
    } else {
        value.saturating_sub(1).max(floor)
    }
}

/// The largest value a workshop number box will hold, from the digits it
/// accepts: the arrows and the keyboard have to agree on the same range.
fn workshop_ceiling(field: LauncherField) -> u32 {
    match workshop_digit_limit(field) {
        Some(digits) => 10u32.saturating_pow(digits as u32) - 1,
        None => u32::MAX,
    }
}

/// Every filesystem the pickers offer, unformatted first.
/// What the picker's first row offers, in the order it draws them.
/// `Unformatted` is a real choice, not an absent one: the volume is left
/// for the Amiga to format itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsFamily {
    Unformatted,
    Ofs,
    Ffs,
}

impl FsFamily {
    pub const ALL: [FsFamily; 3] = [FsFamily::Unformatted, FsFamily::Ofs, FsFamily::Ffs];

    pub fn label(self) -> &'static str {
        match self {
            FsFamily::Unformatted => "Unformatted",
            FsFamily::Ofs => "OFS",
            FsFamily::Ffs => "FFS",
        }
    }

    /// Whether the DOS type carries identifiers to choose from, which an
    /// unformatted volume has no room for -- it has no boot block to tag.
    pub fn has_identifiers(self) -> bool {
        matches!(self, FsFamily::Ofs | FsFamily::Ffs)
    }

    /// The family a chosen filesystem belongs to.
    pub fn of(fs: Option<crate::diskimage::FileSystem>) -> FsFamily {
        match fs {
            None => FsFamily::Unformatted,
            Some(fs) if fs.ffs => FsFamily::Ffs,
            Some(_) => FsFamily::Ofs,
        }
    }
}

/// What a ROM row's chosen image turned out to be, remembered against the
/// path it was read from. Identification opens and checksums the file, which
/// a redraw must never do, so it happens only when the path in the field
/// changes (see [`LauncherState::sync_rom_notes`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RomNote {
    path: Option<PathBuf>,
    text: Option<String>,
    /// Whether the slot has been synced at all: the bundled default
    /// (path None) must still seed its cells once.
    seeded: bool,
    /// The identification split for the tab's table: Name, Version and
    /// Revision cells, computed when the path changes rather than per
    /// draw (the bundled AROS reads its numbers off the image file).
    cells: (String, String, String),
}

/// The full interactive state of the open configuration panel.
#[derive(Debug, Clone)]
pub struct LauncherState {
    pub setup: MachineSetup,
    /// Whether the Save dialog is up: Save As, Set default, Reset default.
    /// One flag, because it holds nothing -- three buttons and a close
    /// gadget, and every click either picks one or puts it away.
    pub save_dialog: bool,
    /// Whether the "are you sure" over Reset default is up. Only ever set
    /// when there is a default to delete -- with none saved there is
    /// nothing to be sure about, and a dialog asking anyway is a dialog
    /// that teaches people to dismiss dialogs.
    pub confirm_reset: bool,
    /// What the Create Image pages will make. Not machine configuration, so
    /// it sits beside the setup rather than inside it.
    pub workshop: ImageWorkshop,
    pub tab: LauncherTab,
    pub status: Option<StatusMessage>,
    /// The Kickstart / extended ROM identifications shown on the ROM tab,
    /// in that order.
    rom_notes: [RomNote; 2],
    /// The text field being typed into, and the edit buffer, when one has
    /// focus (a plugin string option or a drive volume name).
    editing: Option<EditTarget>,
    edit_buffer: String,
    /// Where typing goes in `edit_buffer`.
    edit_caret: Caret,
    /// The Library page: the games found, which is chosen, and where the
    /// list is scrolled to. Held here rather than in the setup for the
    /// same reason the workshop is -- none of it describes a machine.
    #[cfg(feature = "game-library")]
    pub library: LibraryPage,
    /// The signed-in OpenRetro session, for as long as the launcher is
    /// open. Shared with a running scan rather than handed over, so the
    /// row can still say it is signed in while one is going.
    #[cfg(feature = "game-library")]
    pub openretro: Option<std::sync::Arc<crate::gamelib::openretro::Session>>,
    /// The sign-in dialog, when it is up.
    #[cfg(feature = "game-library")]
    pub login: Option<LoginDialog>,
    /// The metadata editor, when it is up.
    #[cfg(feature = "game-library")]
    pub meta: Option<MetaDialog>,
}

/// What the Library page is showing.
#[cfg(feature = "game-library")]
#[derive(Debug, Default, Clone)]
pub struct LibraryPage {
    /// The game database, read once when the page is first opened.
    pub db: crate::gamelib::Database,
    /// Whether the database has been read yet, so an empty one is not
    /// re-read on every frame.
    pub db_loaded: bool,
    /// The games found beside the chosen one.
    pub games: crate::gamelib::Library,
    /// Which of the two lists the keyboard is walking, and which row is
    /// chosen in it. Only the focused list draws a chosen row: a game
    /// highlighted in both at once reads as two selections.
    pub focus: LibraryFocus,
    /// Which entry is chosen, as an index into `games`.
    pub selected: usize,
    /// Which row of the favourites list is chosen, as a position in it.
    pub favourite_selected: usize,
    /// The first row drawn, for scrolling.
    pub scroll: usize,
    /// The same for the favourites list, which scrolls on its own: a
    /// collection can be favourited past the handful of rows the box shows.
    pub favourite_scroll: usize,
    /// How fast a continued scroll is running. One per list, so leaving one
    /// part-way through and picking up the other starts the other at a row
    /// a notch rather than at whatever speed the first had reached.
    pub scroll_rate: ScrollRate,
    pub favourite_scroll_rate: ScrollRate,
    /// Cover art, fetched for whatever is selected and kept afterwards.
    pub covers: crate::gamelib::Covers,
}

/// A package's own name, without the folders above it or its extension.
#[cfg(feature = "game-library")]
fn file_stem_of(relative: &str) -> &str {
    let base = relative.rsplit(['/', '\\']).next().unwrap_or(relative);
    base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base)
}

/// Which of the Library page's two lists is being walked.
#[cfg(feature = "game-library")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LibraryFocus {
    #[default]
    Games,
    Favourites,
}

/// One field of the metadata editor.
#[cfg(feature = "game-library")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MetaField {
    #[default]
    Name,
    Year,
    Publisher,
    Developer,
    Players,
    Version,
}

#[cfg(feature = "game-library")]
impl MetaField {
    pub const ALL: [MetaField; 6] = [
        MetaField::Name,
        MetaField::Year,
        MetaField::Publisher,
        MetaField::Developer,
        MetaField::Players,
        MetaField::Version,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MetaField::Name => "Name",
            MetaField::Year => "Year",
            MetaField::Publisher => "Publisher",
            MetaField::Developer => "Developer",
            MetaField::Players => "Players",
            MetaField::Version => "Version",
        }
    }

    fn at(self) -> usize {
        MetaField::ALL.iter().position(|&f| f == self).unwrap_or(0)
    }
}

/// The metadata editor, while it is open.
///
/// Everything in it is a copy: nothing reaches the store until Save, so
/// Cancel really is "leave it as it was", and Clear is something you can
/// change your mind about.
#[cfg(feature = "game-library")]
#[derive(Debug, Clone, Default)]
pub struct MetaDialog {
    /// The package being edited, by the name the store files it under.
    pub file: String,
    /// The values, in [`MetaField::ALL`] order.
    pub values: [String; 6],
    /// The cache key of the art, which is a catalogue digest for art that
    /// was downloaded and a `manual-` name for art somebody chose.
    pub art: Option<String>,
    pub focus: MetaField,
    /// Where typing goes in the focused field. Metadata is more often
    /// amended than typed fresh, so being able to step into what is already
    /// there is most of the point of the editor.
    pub caret: Caret,
}

#[cfg(feature = "game-library")]
impl MetaDialog {
    pub fn value(&self, field: MetaField) -> &str {
        &self.values[field.at()]
    }

    pub fn value_mut(&mut self, field: MetaField) -> &mut String {
        &mut self.values[field.at()]
    }

    /// Move the focus to another box, putting the caret at the end of what
    /// that one holds.
    pub fn focus_on(&mut self, field: MetaField) {
        self.focus = field;
        self.caret = Caret::end_of(self.value(field));
    }

    /// Type into the focused box at the caret, up to what it accepts.
    pub fn insert(&mut self, c: char, most: usize) {
        if self.value(self.focus).chars().count() >= most {
            return;
        }
        let focus = self.focus;
        let mut caret = self.caret;
        caret.insert(self.value_mut(focus), c);
        self.caret = caret;
    }

    /// Step the caret through the focused box.
    pub fn caret_move(&mut self, to: CaretMove) {
        let len = self.value(self.focus).chars().count();
        match to {
            CaretMove::Left => self.caret.left(),
            CaretMove::Right => self.caret.right_of(len),
            CaretMove::Home => self.caret.home(),
            CaretMove::End => self.caret.end_of_len(len),
        }
    }

    /// Whether there is anything left to save. An editor emptied and saved
    /// hands the package back to the scan rather than pinning it empty.
    pub fn is_empty(&self) -> bool {
        self.art.is_none() && self.values.iter().all(|v| v.trim().is_empty())
    }
}

/// Which box of the sign-in dialog is being typed into.
#[cfg(feature = "game-library")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginField {
    User,
    Pass,
}

/// The sign-in dialog, while it is open.
///
/// Nothing here is kept: the password is traded for a token when OK is
/// pressed and wiped on the way out, and the token lives no longer than the
/// session. See [`crate::gamelib::Secret`] for what that costs to do
/// properly.
#[cfg(feature = "game-library")]
#[derive(Debug)]
pub struct LoginDialog {
    pub user: String,
    pub pass: crate::gamelib::Secret,
    pub focus: LoginField,
    /// Where typing goes in the focused box, as in the metadata editor: one
    /// dialog behaving differently from the other is one more thing to
    /// learn. It steps over the mask in the password box, which is what the
    /// mask is drawn a character at a time for.
    pub caret: Caret,
    /// Set while the sign-in request is in flight, so a second Return does
    /// not start a second one.
    pub sending: bool,
}

/// A cloned launcher state does not carry a half-typed password. That is
/// the only sensible thing a copy of a credential could be, and it is why
/// this is written rather than derived.
#[cfg(feature = "game-library")]
impl Clone for LoginDialog {
    fn clone(&self) -> LoginDialog {
        LoginDialog::default()
    }
}

#[cfg(feature = "game-library")]
impl Default for LoginDialog {
    fn default() -> LoginDialog {
        LoginDialog {
            user: String::new(),
            pass: crate::gamelib::Secret::new(),
            focus: LoginField::User,
            caret: Caret::default(),
            sending: false,
        }
    }
}

#[cfg(feature = "game-library")]
impl LoginDialog {
    /// How many characters the focused box holds. The password is counted
    /// rather than read: the caret needs its length, not its text.
    fn focused_len(&self) -> usize {
        match self.focus {
            LoginField::User => self.user.chars().count(),
            LoginField::Pass => self.pass.chars(),
        }
    }

    /// Move to the other box, with the caret at the end of what it holds.
    pub fn focus_on(&mut self, field: LoginField) {
        self.focus = field;
        self.caret = Caret::at_end_of_len(self.focused_len());
    }

    /// Type at the caret, up to what the box takes.
    pub fn insert(&mut self, c: char) {
        match self.focus {
            LoginField::User if self.user.chars().count() < USER_MAX => {
                let mut caret = self.caret;
                caret.insert(&mut self.user, c);
                self.caret = caret;
            }
            // Full: the character is dropped and the caret stays put.
            LoginField::User => {}
            // The Secret bounds itself, and says nothing about how full it
            // is, so the caret follows only a character that went in.
            LoginField::Pass => {
                let was = self.pass.chars();
                self.pass.insert_at(self.caret.at(), c);
                if self.pass.chars() == was {
                    return;
                }
                self.caret.right_of(self.pass.chars());
            }
        }
    }

    /// Delete backwards from the caret.
    pub fn backspace(&mut self) {
        if self.caret.at() == 0 {
            return;
        }
        match self.focus {
            LoginField::User => {
                let mut caret = self.caret;
                caret.backspace(&mut self.user);
                self.caret = caret;
            }
            LoginField::Pass => {
                self.caret.left();
                self.pass.remove_at(self.caret.at());
            }
        }
    }

    /// Delete the character the caret is on.
    pub fn delete(&mut self) {
        match self.focus {
            LoginField::User => {
                let mut caret = self.caret;
                caret.delete(&mut self.user);
                self.caret = caret;
            }
            LoginField::Pass => {
                self.pass.remove_at(self.caret.at());
            }
        }
    }

    /// Step the caret through the focused box.
    pub fn caret_move(&mut self, to: CaretMove) {
        let len = self.focused_len();
        match to {
            CaretMove::Left => self.caret.left(),
            CaretMove::Right => self.caret.right_of(len),
            CaretMove::Home => self.caret.home(),
            CaretMove::End => self.caret.end_of_len(len),
        }
    }
}

/// The longest an OpenRetro user name is taken to be. Nothing documents a
/// limit; this is a box, not a validator, and a name past it is a paste
/// that went somewhere it did not belong.
#[cfg(feature = "game-library")]
const USER_MAX: usize = 64;

/// The support directory under a given configuration directory, from
/// `paths` so this and the no-argument helpers cannot describe different
/// trees. Kept as a name here because the call sites read better for it.
#[cfg(feature = "game-library")]
fn whdload_support_under(config_dir: &std::path::Path) -> PathBuf {
    crate::paths::whdload_support_in(config_dir)
}

#[cfg(feature = "game-library")]
impl MachineSetup {
    /// Where the scanned library is kept: `[whdload] library_db`, or the
    /// default under the configuration directory.
    pub fn library_db(&self, config_dir: &std::path::Path) -> PathBuf {
        self.whdload_library_db
            .clone()
            .unwrap_or_else(|| whdload_support_under(config_dir).join("launcher.db"))
    }

    /// Where a scan keeps what it downloaded: `[whdload] library_cache`, or
    /// the default beside the library it belongs to.
    pub fn library_cache(&self, config_dir: &std::path::Path) -> PathBuf {
        self.whdload_library_cache
            .clone()
            .unwrap_or_else(|| whdload_support_under(config_dir).join("cache"))
    }

    /// Adopt whatever is already sitting where WHDLoad would have put it.
    ///
    /// A person who has run the packaging script, or downloaded once and
    /// then started a fresh configuration, should not have to point at the
    /// same files again. Only fills a setting that is empty, so a chosen
    /// path is never quietly replaced -- and only adopts an archive whose
    /// digest is right, since a half-finished download in the right place
    /// with the right name is exactly what should *not* be adopted.
    pub fn adopt_whdload_defaults(&mut self) {
        for archive in crate::gamelib::support::Archive::ALL {
            let field = match archive {
                crate::gamelib::support::Archive::Whdload => F::WhdloadWhdPackage,
                crate::gamelib::support::Archive::Skick => F::WhdloadSkickPackage,
            };
            if self.path(field).is_none() {
                if let Some(at) = archive.found_locally() {
                    self.set_path(field, at);
                }
            }
        }
        // Kickstart images are somebody's own collection rather than a
        // known file, so the test is that the directory has anything in it
        // at all.
        if self.path(F::WhdloadKickstarts).is_none() {
            if let Some(dir) = crate::gamelib::support::default_kickstart_dir() {
                if crate::gamelib::support::holds_anything(&dir) {
                    self.set_path(F::WhdloadKickstarts, dir);
                }
            }
        }
    }
}

#[cfg(feature = "game-library")]
impl LauncherState {
    /// Bring the Library page up to date with what the configuration says.
    ///
    /// Called when the page is drawn or clicked rather than when the
    /// settings change: the game and the database path are ordinary path
    /// fields that anything may set -- a Browse, a drop, a loaded config --
    /// and one place that notices beats a dozen that have to remember to.
    pub fn refresh_library(&mut self, config_dir: &std::path::Path) {
        // Turned off, the page does nothing at all: no store read, no
        // cover worker started, nothing held. Whatever a previous session
        // left in memory goes with it.
        if !self.setup.whdload_enabled() {
            if self.library.db_loaded {
                self.library = LibraryPage::default();
            }
            return;
        }
        // No game library means nothing to list -- and nothing to show in
        // the favourites either. They are kept in the store, so without
        // this a fresh install with no library set still listed whatever a
        // previous one had starred.
        if self.library_folder().is_none() {
            if self.library.db_loaded {
                self.library = LibraryPage::default();
            }
            return;
        }
        if !self.library.db_loaded {
            self.library.db = crate::gamelib::Database::load(&self.setup.library_db(config_dir));
            // The covers subdirectory of the cache, not the cache itself:
            // that is where a scan writes them.
            self.library.covers = crate::gamelib::Covers::new(crate::gamelib::scan::covers_path(
                &self.setup.library_cache(config_dir),
            ));
            self.library.db_loaded = true;
        }
        // From the store, not from the disk: opening a page is not a
        // reason to walk a collection of several thousand packages. The
        // Refresh button is.
        match self.library_folder() {
            // A store built from another folder is not this folder's list:
            // its entries are paths under somewhere else, and showing them
            // here offers games that are not there. Refresh is what reads
            // the folder and makes the store this one's.
            Some(folder) if !self.library.db.lists(&folder) => {
                self.library.games = crate::gamelib::Library::default();
            }
            Some(folder) if !self.library.games.covers(&folder) => {
                self.library.games = crate::gamelib::Library::known(&folder, &self.library.db);
                self.select_running_game();
            }
            None => self.library.games = crate::gamelib::Library::default(),
            _ => {}
        }
        // A list that shrank under the selection must not leave it past the
        // end, where nothing would be drawn as chosen.
        self.library.selected = self
            .library
            .selected
            .min(self.library.games.len().saturating_sub(1));
    }

    /// Put the selection on the game the configuration names, so opening
    /// the page while one is running shows which.
    fn select_running_game(&mut self) {
        if let Some(at) = self
            .setup
            .path(F::WhdloadGame)
            .and_then(|game| self.library.games.position(game))
        {
            self.library.selected = at;
        }
    }

    /// The entry the list is on, if the list has any.
    /// Keep the cover art up with the selection: collect anything that has
    /// arrived, and ask for what is being looked at now. Answers whether
    /// the page changed and should be drawn again.
    ///
    /// Here rather than at each place a game is chosen, because every route
    /// to a selection -- a click, an arrow key, the favourites list, a
    /// config that named a game -- has to end up asking.
    pub fn poll_library_covers(&mut self) -> bool {
        let arrived = self.library.covers.poll();
        // The selection and the few either side of it. Reading ahead is
        // what makes a steady scroll find each cover already decoded
        // instead of waiting for it at every step.
        let at = self.library.selected;
        let entries = self.library.games.entries();
        let window: Vec<Option<&str>> = entries
            .iter()
            .map(|entry| {
                entry
                    .game
                    .as_ref()
                    .and_then(|game| game.front_sha1.as_deref())
            })
            .collect();
        self.library.covers.want_around(window.into_iter(), at);
        // And whatever the editor is showing, which after a picture has
        // been chosen is not the selection's art any more.
        if let Some(key) = self.meta.as_ref().and_then(|meta| meta.art.clone()) {
            self.library.covers.want(&key);
        }
        arrived
    }

    /// The folder the list is of.
    ///
    /// A game library is a collection and is searched all the way down;
    /// without one, the folder holding the chosen game stands in, so
    /// pointing at any package still lists its neighbours. The library
    /// wins where both are set: it is the deliberate answer to "where are
    /// my games", and the other is a guess from one of them.
    /// The folder the Library page lists, which is the one that was set
    /// and nothing else.
    ///
    /// It used to fall back to the folder holding the chosen game, so that
    /// pointing at a package listed its neighbours. That made Clear look
    /// broken -- emptying the setting left the list full of whatever sat
    /// beside the launch game -- and it hid the one thing the empty page
    /// has to say, which is where to set the folder.
    pub fn library_folder(&self) -> Option<PathBuf> {
        self.setup
            .path(F::WhdloadGames)
            .map(std::path::Path::to_path_buf)
    }

    pub fn library_selection(&self) -> Option<&crate::gamelib::Entry> {
        match self.library.focus {
            LibraryFocus::Games => self.library.games.entries().get(self.library.selected),
            // A favourite whose package is no longer there has no entry,
            // and so nothing to show beside it.
            LibraryFocus::Favourites => {
                let key = self.favourite_key(self.library.favourite_selected)?;
                self.library
                    .games
                    .entries()
                    .iter()
                    .find(|entry| entry.relative == key)
            }
        }
    }

    /// The key of the favourite on that row.
    pub fn favourite_key(&self, drawn: usize) -> Option<&str> {
        self.library.db.favourites().nth(drawn).map(|(key, _)| key)
    }

    /// Choose a game, which is also to say launch it when Run is pressed:
    /// the selection *is* the configured game rather than something that
    /// has to be copied across on the way out.
    pub fn select_library_game(&mut self, index: usize) {
        let Some(entry) = self.library.games.entries().get(index) else {
            return;
        };
        let path = entry.path.clone();
        self.library.focus = LibraryFocus::Games;
        self.library.selected = index;
        self.setup.set_path(F::WhdloadGame, path);
        self.status = None;
    }

    /// Mark or unmark the game at `index` in the list.
    pub fn toggle_library_favourite(&mut self, index: usize) {
        let Some(entry) = self.library.games.entries().get(index) else {
            return;
        };
        // The name is kept with the mark, so the favourite still reads as
        // a game once its package is gone.
        // The package, not the game: a collection holds the same game
        // several times over, and starring one of them stars that one.
        let (file, title) = (entry.relative.clone(), entry.title().to_string());
        self.library.db.toggle_favourite(&file, &title);
    }

    /// Choose a favourite: the game to launch, without also being the row
    /// highlighted in the list above. Two highlights for one choice is
    /// two choices as far as anyone reading the page is concerned.
    pub fn select_favourite(&mut self, drawn: usize) {
        let Some(key) = self.favourite_key(drawn).map(str::to_string) else {
            return;
        };
        self.library.focus = LibraryFocus::Favourites;
        self.library.favourite_selected = drawn;
        self.status = None;
        // A favourite whose package has been deleted is still a row worth
        // landing on -- its Remove tick is the point of it -- but there is
        // nothing to launch.
        let path = self
            .library
            .games
            .entries()
            .iter()
            .find(|entry| entry.relative == key)
            .map(|entry| entry.path.clone());
        if let Some(path) = path {
            self.setup.set_path(F::WhdloadGame, path);
        }
    }

    /// Open the metadata editor on whatever is selected.
    pub fn open_meta_editor(&mut self) -> bool {
        let Some(entry) = self.library_selection() else {
            return false;
        };
        // The store keys on the path under the game folder; the version
        // offered below is the package's own name, which is what tells one
        // release from another. Without its extension -- `.lha` against
        // `.zip` is how it was packed, not which release it is.
        let file = entry.relative.clone();
        let base = entry.file_name.clone();
        let duplicated = entry.duplicated;
        let game = entry.game.clone();
        let mut dialog = MetaDialog {
            file,
            focus: MetaField::Name,
            art: game.as_ref().and_then(|g| g.front_sha1.clone()),
            ..Default::default()
        };
        if let Some(game) = &game {
            for (field, value) in [
                (MetaField::Name, Some(game.name.clone())),
                (MetaField::Year, game.year.clone()),
                (MetaField::Publisher, game.publisher.clone()),
                (MetaField::Developer, game.developer.clone()),
                (MetaField::Players, game.players.clone()),
                (MetaField::Version, game.version.clone()),
            ] {
                *dialog.value_mut(field) = value.unwrap_or_default();
            }
        }
        // Nothing stored, the game has metadata, and the library holds it
        // more than once: offer the file name, which is the only thing that
        // separates them. Blank for a game held once, and blank for one the
        // scan could not name at all -- filling in a version for a package
        // with nothing else known about it says nothing.
        if dialog.value(MetaField::Version).is_empty() && duplicated && game.is_some() {
            *dialog.value_mut(MetaField::Version) = base;
        }
        // The caret goes to the end of the first box, which is where typing
        // would go anyway and where the block is expected to be.
        dialog.focus_on(MetaField::Name);
        self.meta = Some(dialog);
        self.status = None;
        true
    }

    /// Commit the editor into the store, and answer where to save it.
    ///
    /// An editor with nothing in it clears the entry and hands it back to
    /// the scan; anything else is marked as hand-filled, which is what
    /// stops the next scan overwriting it.
    pub fn commit_meta_editor(&mut self) {
        let Some(dialog) = self.meta.take() else {
            return;
        };
        // Keeping the digest means a later scan still recognises the
        // package without opening it again.
        let held_digest = self
            .library
            .db
            .entry(&dialog.file)
            .and_then(|k| k.slave_sha1.clone());
        let field = |f: MetaField| {
            let value = dialog.value(f).trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let entry = if dialog.is_empty() {
            crate::gamelib::Known {
                file: dialog.file.clone(),
                game: None,
                slave_sha1: held_digest,
                manual: false,
            }
        } else {
            crate::gamelib::Known {
                file: dialog.file.clone(),
                game: Some(crate::gamelib::Game {
                    // Kept, so a re-sync can still recognise the record
                    // this started life as.
                    uuid: self
                        .library
                        .db
                        .entry(&dialog.file)
                        .and_then(|k| k.game.as_ref())
                        .map(|g| g.uuid.clone())
                        .unwrap_or_default(),
                    // A name left blank falls back to what the list would
                    // have shown anyway -- the file's own name, without
                    // the folders it sits in or its extension.
                    name: field(MetaField::Name)
                        .unwrap_or_else(|| file_stem_of(&dialog.file).to_string()),
                    year: field(MetaField::Year),
                    publisher: field(MetaField::Publisher),
                    developer: field(MetaField::Developer),
                    players: field(MetaField::Players),
                    version: field(MetaField::Version),
                    front_sha1: dialog.art.clone(),
                }),
                slave_sha1: held_digest,
                manual: true,
            }
        };
        self.library.db.set_entry(entry);
    }

    /// Move the selection in whichever list is focused.
    pub fn step_library_focus(&mut self, delta: isize, visible: usize) {
        match self.library.focus {
            LibraryFocus::Games => self.step_library(delta, visible),
            LibraryFocus::Favourites => {
                let len = self.library.db.favourite_count();
                if len == 0 {
                    return;
                }
                let at = (self.library.favourite_selected as isize + delta)
                    .clamp(0, len as isize - 1) as usize;
                self.select_favourite(at);
                self.scroll_favourites_into_view(visible);
            }
        }
    }

    /// Take a favourite off from the favourites list itself.
    pub fn remove_favourite(&mut self, at: usize) {
        if let Some(key) = self.favourite_key(at).map(str::to_string) {
            self.library.db.remove_favourite(&key);
        }
        // The list just got shorter under both the selection and the scroll,
        // either of which can now be off the end of it.
        self.clamp_favourites();
    }

    /// Keep the favourites selection and scroll inside a list that may have
    /// shrunk -- a removal here, or a database re-read that dropped some.
    fn clamp_favourites(&mut self) {
        let last = self.library.db.favourite_count().saturating_sub(1);
        self.library.favourite_selected = self.library.favourite_selected.min(last);
        self.library.favourite_scroll = self.library.favourite_scroll.min(last);
    }

    /// Re-read the game folder: the one thing that walks the disk, and the
    /// only way a package that was not there before gets into the store.
    ///
    /// What it finds goes into the store rather than only into the list, so
    /// the page shows the same thing when it is next opened and after a
    /// restart. Metadata already resolved is carried across -- reading the
    /// folder again must not be a way to lose the work a scan did.
    ///
    /// Answers how many packages are in the folder, and how many of those
    /// have nothing known about them yet.
    pub fn rescan_library(&mut self, config_dir: &std::path::Path) -> (usize, usize) {
        self.refresh_library(config_dir);
        let Some(folder) = self.library_folder() else {
            return (0, 0);
        };
        let fresh = self
            .library
            .db
            .merge_found(crate::gamelib::scan::packages(&folder));
        self.library.db.set_folder(&folder);
        self.save_library_database(config_dir);
        self.library.games = crate::gamelib::Library::known(&folder, &self.library.db);
        self.select_running_game();
        self.library.selected = self
            .library
            .selected
            .min(self.library.games.len().saturating_sub(1));
        (self.library.games.len(), fresh)
    }

    /// Write the database out, so a favourite outlives the session.
    pub fn save_library_database(&self, config_dir: &std::path::Path) {
        let at = self.setup.library_db(config_dir);
        if let Err(e) = self.library.db.save(&at) {
            log::warn!("game library: could not save {}: {e}", at.display());
        }
    }

    /// Move the selection by `delta` rows, keeping it in view.
    pub fn step_library(&mut self, delta: isize, visible: usize) {
        let len = self.library.games.len();
        if len == 0 {
            return;
        }
        let at = (self.library.selected as isize + delta).clamp(0, len as isize - 1) as usize;
        self.select_library_game(at);
        self.scroll_library_into_view(visible);
    }

    /// Scroll the list by `delta` rows without moving the selection.
    pub fn scroll_library(&mut self, delta: isize, visible: usize) {
        let last_start = self.library.games.len().saturating_sub(visible);
        let at = self.library.scroll as isize + delta;
        self.library.scroll = at.clamp(0, last_start as isize) as usize;
    }

    /// Which buckets the list has games in, so the row can grey the rest.
    pub fn az_buckets_present(&self) -> [bool; AZ_BUCKETS] {
        let mut present = [false; AZ_BUCKETS];
        for entry in self.library.games.entries() {
            if let Some(slot) = present.get_mut(az_bucket_of(entry.title())) {
                *slot = true;
            }
        }
        present
    }

    /// Jump the list to the first game in a bucket, and choose it.
    pub fn jump_to_bucket(&mut self, bucket: usize, visible: usize) {
        let Some(at) = self
            .library
            .games
            .entries()
            .iter()
            .position(|entry| az_bucket_of(entry.title()) == bucket)
        else {
            return;
        };
        self.select_library_game(at);
        // The letter's first game at the top of the box rather than merely
        // on screen: the point of the jump is to see what is under it.
        let last_start = self.library.games.len().saturating_sub(visible);
        self.library.scroll = at.min(last_start);
    }

    pub fn scroll_favourites(&mut self, delta: isize, visible: usize) {
        let last_start = self.library.db.favourite_count().saturating_sub(visible);
        let at = self.library.favourite_scroll as isize + delta;
        self.library.favourite_scroll = at.clamp(0, last_start as isize) as usize;
    }

    fn scroll_favourites_into_view(&mut self, visible: usize) {
        let at = self.library.favourite_selected;
        if at < self.library.favourite_scroll {
            self.library.favourite_scroll = at;
        } else if visible > 0 && at >= self.library.favourite_scroll + visible {
            self.library.favourite_scroll = at + 1 - visible;
        }
    }

    /// Bring the selection into the drawn rows, moving as little as will do.
    fn scroll_library_into_view(&mut self, visible: usize) {
        let at = self.library.selected;
        if at < self.library.scroll {
            self.library.scroll = at;
        } else if visible > 0 && at >= self.library.scroll + visible {
            self.library.scroll = at + 1 - visible;
        }
    }
}

impl LauncherState {
    /// Whether a field belongs to the Create Image workshop rather than to
    /// the machine, so the drawing and click paths read the right state.
    pub fn is_workshop(field: LauncherField) -> bool {
        matches!(
            field,
            F::NewFloppyDensity
                | F::NewFloppyContainer
                | F::NewFloppyFs
                | F::NewFloppyFsVariant
                | F::NewFloppyLabel
                | F::NewFloppyBootable
                | F::NewFloppyCreate
                | F::NewHardSize
                | F::NewHardGeometryMode
                | F::NewHardPartitioning
                | F::NewHardDevice
                | F::NewHardFs
                | F::NewHardFsVariant
                | F::NewHardLabel
                | F::NewHardBootable
                | F::NewHardBootPri
                | F::NewHardReadOnly
                | F::NewHardSparse
                | F::NewHardCreate
                | F::NewGeomCylinders
                | F::NewGeomSurfaces
                | F::NewGeomSectors
                | F::NewGeomReserved
                | F::NewGeomVendor
                | F::NewGeomProduct
                | F::NewGeomRevision
                | F::NewGeomSave
                | F::NewGeomAuto
        )
    }

    /// Whether a field is one of the Serial section's TCP address boxes.
    /// They share the Create Image pages' free-text widget but hold machine
    /// configuration, so the drawing and click paths have to tell them apart.
    pub fn is_serial_addr(field: LauncherField) -> bool {
        #[cfg(feature = "midi")]
        {
            matches!(field, F::SerialConnect | F::SerialListen)
        }
        #[cfg(not(feature = "midi"))]
        {
            let _ = field;
            false
        }
    }

    /// Whether the value box for `field` is the one being typed into. Both
    /// the Create Image boxes and the serial address boxes are drawn by the
    /// same widget, so it asks this one question of either.
    pub fn typing_in_value_box(&self, field: LauncherField) -> bool {
        matches!(
            self.editing,
            Some(EditTarget::NewImageText(f) | EditTarget::SerialAddr(f)) if f == field
        ) || field == F::RamPattern && self.editing == Some(EditTarget::RamPattern)
    }

    /// The filesystem a Create Image row is about: the floppy page's or the
    /// hard-drive page's, whichever row asked.
    pub fn workshop_fs_of(&self, field: LauncherField) -> Option<crate::diskimage::FileSystem> {
        match field {
            F::NewHardFs | F::NewHardFsVariant => self.workshop.hard_fs,
            _ => self.workshop.floppy_fs,
        }
    }

    /// Whether a family tick box is the chosen one.
    pub fn workshop_fs_family_set(&self, field: LauncherField, family: FsFamily) -> bool {
        FsFamily::of(self.workshop_fs_of(field)) == family
    }

    /// Whether a DOSType tick box shows as set.
    ///
    /// More than one can be: dircache and longname each *are* international,
    /// so choosing either lights the International box too. What cannot
    /// happen is dircache and longname together -- they are two values of
    /// one field.
    pub fn workshop_fs_variant_set(
        &self,
        field: LauncherField,
        variant: crate::diskimage::Variant,
    ) -> bool {
        use crate::diskimage::Variant as V;
        let Some(fs) = self.workshop_fs_of(field) else {
            return false;
        };
        match variant {
            V::Intl => fs.variant.is_intl(),
            V::DirCache => fs.variant.is_dircache(),
            V::LongName => fs.variant.is_longname(),
            V::Plain => false,
        }
    }

    /// Whether a DOSType tick box can be clicked.
    ///
    /// International is fixed on while a directory scheme is chosen: it
    /// comes with them, and there is no tag that has one without it. And
    /// each directory scheme turns the other away, because the tag holds
    /// one or neither.
    pub fn workshop_fs_variant_enabled(
        &self,
        field: LauncherField,
        variant: crate::diskimage::Variant,
    ) -> bool {
        use crate::diskimage::Variant as V;
        let Some(fs) = self.workshop_fs_of(field) else {
            return false;
        };
        match variant {
            V::Intl => !fs.variant.is_dircache() && !fs.variant.is_longname(),
            V::DirCache => !fs.variant.is_longname(),
            V::LongName => !fs.variant.is_dircache(),
            V::Plain => false,
        }
    }

    /// Choose a filesystem family, keeping the variant that was already
    /// picked: moving between OFS and FFS is a change of one bit, and
    /// silently dropping "international" with it would be a surprise.
    pub fn workshop_set_fs_family(&mut self, field: LauncherField, family: FsFamily) {
        let variant = self
            .workshop_fs_of(field)
            .map(|fs| fs.variant)
            .unwrap_or_default();
        let chosen = match family {
            FsFamily::Unformatted => None,
            FsFamily::Ofs => Some(crate::diskimage::FileSystem {
                ffs: false,
                variant,
            }),
            FsFamily::Ffs => Some(crate::diskimage::FileSystem { ffs: true, variant }),
        };
        match field {
            F::NewHardFs | F::NewHardFsVariant => self.workshop.hard_fs = chosen,
            _ => self.workshop.floppy_fs = chosen,
        }
    }

    /// Tick or clear one DOSType box, landing on whichever tag the boxes
    /// then describe.
    ///
    /// Clearing a directory scheme leaves International behind rather than
    /// going all the way back to plain: the box is still lit, so the state
    /// the user is looking at is the state they get.
    pub fn workshop_set_fs_variant(
        &mut self,
        field: LauncherField,
        variant: crate::diskimage::Variant,
    ) {
        use crate::diskimage::Variant as V;
        if !self.workshop_fs_variant_enabled(field, variant) {
            return;
        }
        let Some(mut fs) = self.workshop_fs_of(field) else {
            return;
        };
        let set = self.workshop_fs_variant_set(field, variant);
        fs.variant = match (variant, set) {
            (V::Intl, true) => V::Plain,
            (V::Intl, false) => V::Intl,
            (V::DirCache | V::LongName, true) => V::Intl,
            (V::DirCache, false) => V::DirCache,
            (V::LongName, false) => V::LongName,
            (V::Plain, _) => fs.variant,
        };
        match field {
            F::NewHardFs | F::NewHardFsVariant => self.workshop.hard_fs = Some(fs),
            _ => self.workshop.floppy_fs = Some(fs),
        }
    }

    /// A row's displayed value, from wherever that row's state lives.
    pub fn row_value(&self, field: LauncherField) -> String {
        if Self::is_workshop(field) {
            self.workshop_value(field)
        } else {
            self.setup.value_label(field)
        }
    }

    /// Whether a row's tick box is on, from wherever it lives.
    pub fn row_toggle(&self, field: LauncherField) -> bool {
        if Self::is_workshop(field) {
            self.workshop_toggle(field)
        } else {
            self.setup.toggle_value(field)
        }
    }

    /// Whether a row can be used at all, from wherever it lives.
    pub fn row_applies(&self, field: LauncherField) -> bool {
        if Self::is_workshop(field) {
            self.workshop_applies(field)
        } else {
            self.setup.applies(field)
        }
    }

    /// The value a Create Image row shows.
    pub fn workshop_value(&self, field: LauncherField) -> String {
        let w = &self.workshop;
        match field {
            F::NewFloppyDensity => w.density.label().to_string(),
            F::NewFloppyContainer => w.container.label().to_string(),
            F::NewFloppyLabel => w.floppy_label.clone(),
            F::NewHardSize => w.size.to_string(),
            F::NewHardBootPri => w.boot_pri.to_string(),
            F::NewGeomCylinders => w.custom_geometry.cylinders.to_string(),
            F::NewGeomSurfaces => w.custom_geometry.surfaces.to_string(),
            F::NewGeomSectors => w.custom_geometry.sectors.to_string(),
            F::NewGeomReserved => w.reserved.to_string(),
            F::NewGeomVendor => w.identity().vendor,
            F::NewGeomProduct => w.identity().product,
            F::NewGeomRevision => w.identity().revision,
            F::NewHardPartitioning => w.partitioning.label().to_string(),
            F::NewHardDevice => w.device.clone(),
            F::NewHardLabel => w.hard_label.clone(),
            _ => String::new(),
        }
    }

    /// Whether a Create Image toggle is on.
    pub fn workshop_toggle(&self, field: LauncherField) -> bool {
        match field {
            F::NewFloppyBootable => self.workshop.floppy_bootable,
            F::NewHardBootable => self.workshop.hard_bootable,
            F::NewHardReadOnly => self.workshop.read_only,
            F::NewHardSparse => self.workshop.sparse,
            _ => false,
        }
    }

    /// Whether a Create Image row can be used at all. Boot code needs a
    /// filesystem to load, so an unformatted disk has nothing to boot, and
    /// a volume name only means something once there is a volume.
    pub fn workshop_applies(&self, field: LauncherField) -> bool {
        let w = &self.workshop;
        match field {
            F::NewFloppyBootable | F::NewFloppyLabel => w.floppy_fs.is_some(),
            F::NewHardLabel => w.hard_fs.is_some(),
            // An unformatted volume has no DOS type, so there is nothing
            // for its identifiers to identify -- label and all.
            F::NewFloppyFsVariant => w.floppy_fs.is_some(),
            F::NewHardFsVariant => w.hard_fs.is_some(),
            // Without a partition table there is no partition entry to
            // carry a device name or a boot flag: the emulator names the
            // mount instead.
            F::NewHardDevice | F::NewHardBootable => {
                w.partitioning == crate::diskimage::Partitioning::Rdb
            }
            // Only a boot candidate has a rank among boot candidates.
            F::NewHardBootPri => {
                w.partitioning == crate::diskimage::Partitioning::Rdb && w.hard_bootable
            }
            _ => true,
        }
    }

    /// The button wording on a Create Image page's action row.
    pub fn workshop_action_label(&self, field: LauncherField) -> String {
        match field {
            // Both write a file, and the dialog that follows says which
            // kind: the page is already headed with that.
            F::NewFloppyCreate | F::NewHardCreate => "Save...".to_string(),
            F::NewGeomSave => "Apply".to_string(),
            F::NewGeomAuto => "Auto".to_string(),
            _ => String::new(),
        }
    }

    /// Step a Create Image picker.
    pub fn workshop_cycle(&mut self, field: LauncherField, forward: bool) {
        let w = &mut self.workshop;
        match field {
            F::NewFloppyDensity => {
                w.density = cycle_slice(&crate::diskimage::Density::ALL, w.density, forward)
            }
            F::NewFloppyContainer => {
                w.container = cycle_slice(&crate::diskimage::Container::ALL, w.container, forward)
            }
            // The geometry figures are the only stepped numbers here: the
            // size and the boot priority are typed, and have no arrows.
            //
            // Cylinder 0 goes to the Rigid Disk Block, so a drive needs a
            // second one before there is anything to partition.
            F::NewGeomCylinders => {
                w.custom_geometry.cylinders = step_u32(
                    w.custom_geometry.cylinders,
                    forward,
                    2,
                    workshop_ceiling(field),
                )
            }
            F::NewGeomSurfaces => {
                w.custom_geometry.surfaces = step_u32(
                    w.custom_geometry.surfaces,
                    forward,
                    1,
                    workshop_ceiling(field),
                )
            }
            F::NewGeomSectors => {
                w.custom_geometry.sectors = step_u32(
                    w.custom_geometry.sectors,
                    forward,
                    1,
                    workshop_ceiling(field),
                )
            }
            // The boot block is two blocks long, so two is the floor: with
            // fewer, the filesystem would be free to allocate over it.
            F::NewGeomReserved => {
                w.reserved = step_u32(
                    w.reserved,
                    forward,
                    crate::diskimage::RESERVED_BLOCKS,
                    workshop_ceiling(field),
                )
            }
            F::NewHardPartitioning => {
                w.partitioning = cycle_slice(
                    &crate::diskimage::Partitioning::ALL,
                    w.partitioning,
                    forward,
                )
            }
            _ => {}
        }
    }

    /// Flip a Create Image tick box.
    pub fn workshop_toggle_flip(&mut self, field: LauncherField) {
        let w = &mut self.workshop;
        match field {
            F::NewFloppyBootable => w.floppy_bootable = !w.floppy_bootable,
            F::NewHardBootable => w.hard_bootable = !w.hard_bootable,
            F::NewHardReadOnly => w.read_only = !w.read_only,
            F::NewHardSparse => w.sparse = !w.sparse,
            _ => {}
        }
    }

    /// Focus a Create Image text field, seeding the buffer with its value.
    pub fn begin_edit_new_image(&mut self, field: LauncherField) {
        if !self.workshop_applies(field) {
            return;
        }
        self.edit_buffer = self.workshop_value(field);
        self.editing = Some(EditTarget::NewImageText(field));
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    /// Focus a serial TCP address box, seeding the buffer with the address
    /// it holds. An unset box starts empty rather than from what it shows:
    /// the default listen address and the "(host:port)" prompt are both
    /// placeholders, not values to be typed over.
    pub fn begin_edit_serial_addr(&mut self, field: LauncherField) {
        if !Self::is_serial_addr(field) {
            return;
        }
        self.edit_buffer.clear();
        #[cfg(feature = "midi")]
        if let Some(addr) = self.setup.serial_addr(field) {
            self.edit_buffer.push_str(addr);
        }
        self.editing = Some(EditTarget::SerialAddr(field));
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    /// Focus the fixed RAM word, using the canonical hexadecimal spelling the
    /// configuration writer will emit.
    pub fn begin_edit_ram_pattern(&mut self) {
        if !self.setup.applies(F::RamPattern) {
            return;
        }
        self.edit_buffer = self.setup.value_label(F::RamPattern);
        self.editing = Some(EditTarget::RamPattern);
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    pub fn new(setup: MachineSetup) -> Self {
        let mut setup = setup;
        // Read the host devices as the screen opens so the pickers show what is
        // connected now.
        setup.refresh_host_devices();
        // Likewise the WHDLoad support files: anything already sitting
        // where they go fills its own row rather than asking again.
        #[cfg(feature = "game-library")]
        setup.adopt_whdload_defaults();
        let mut state = Self {
            save_dialog: false,
            confirm_reset: false,
            #[cfg(feature = "game-library")]
            library: LibraryPage::default(),
            #[cfg(feature = "game-library")]
            openretro: None,
            #[cfg(feature = "game-library")]
            login: None,
            #[cfg(feature = "game-library")]
            meta: None,
            setup,
            workshop: ImageWorkshop::default(),
            tab: LauncherTab::System,
            status: None,
            rom_notes: Default::default(),
            editing: None,
            edit_buffer: String::new(),
            edit_caret: Caret::default(),
        };
        // Name the ROMs the incoming configuration already carries.
        state.sync_rom_notes();
        state
    }

    /// Re-identify the ROM images when the chosen files change.
    ///
    /// Identification opens and checksums the image, so it must never happen
    /// on a redraw: the result is kept against the path it came from and this
    /// does nothing at all while that path stays put. Call it wherever a
    /// control may have changed the configuration (browsing to an image,
    /// clearing a row, resetting to defaults, loading a config file).
    pub fn sync_rom_notes(&mut self) {
        for (slot, field) in [(0, F::Rom), (1, F::ExtendedRom)] {
            let path = self.setup.path(field).map(Path::to_path_buf);
            if self.rom_notes[slot].seeded && self.rom_notes[slot].path == path {
                continue;
            }
            self.rom_notes[slot].text = path.as_deref().and_then(crate::config::rom_identification);
            self.rom_notes[slot].cells = Self::rom_cells_for(
                slot,
                path.as_deref(),
                self.rom_notes[slot].text.as_deref(),
                self.setup.path(F::Rom).is_none(),
            );
            self.rom_notes[slot].path = path;
            self.rom_notes[slot].seeded = true;
        }
    }

    /// The table cells for one slot: a checksum-named image splits its
    /// label; an AROS image -- chosen or bundled -- reads its numbers off
    /// the file itself, so they follow the bundled ROM between releases.
    fn rom_cells_for(
        slot: usize,
        path: Option<&Path>,
        text: Option<&str>,
        main_is_bundled: bool,
    ) -> (String, String, String) {
        let aros_cells = |file: &Path| {
            let (version, revision) = crate::romdb::rom_self_versions(file).unwrap_or_default();
            ("AROS".to_string(), version, revision)
        };
        match (path, text) {
            (Some(p), Some("bundled AROS")) => aros_cells(p),
            (_, Some(note)) => Self::split_identification(note),
            (Some(_), None) => (String::new(), String::new(), String::new()),
            (None, _) => {
                // An empty slot runs the bundled AROS -- the extended
                // half only alongside the bundled main.
                if slot == 1 && !main_is_bundled {
                    return (String::new(), String::new(), String::new());
                }
                let Some(bundle) = crate::romsearch::find_bundled_aros() else {
                    return (String::new(), String::new(), String::new());
                };
                aros_cells(if slot == 0 {
                    &bundle.main
                } else {
                    &bundle.extended
                })
            }
        }
    }

    /// The identification for the ROM tab's table: Name, Version and
    /// Revision cells, from the cache [`Self::sync_rom_notes`] keeps.
    pub fn rom_note_cells(&self, field: LauncherField) -> (String, String, String) {
        let slot = match field {
            F::Rom => 0,
            F::ExtendedRom => 1,
            _ => return (String::new(), String::new(), String::new()),
        };
        self.rom_notes[slot].cells.clone()
    }

    /// Split a checksum label into the table's columns. The labels read
    /// "Kickstart 3.1 (40.68) A1200": name words first, a marketing
    /// version, the ROM's own revision in parentheses, then the models
    /// -- which have no column, and are dropped.
    fn split_identification(note: &str) -> (String, String, String) {
        let mut name_words: Vec<&str> = Vec::new();
        let mut version = String::new();
        let mut revision = String::new();
        for word in note.split_whitespace() {
            if version.is_empty()
                && word.chars().next().is_some_and(|c| c.is_ascii_digit())
                && word.contains('.')
            {
                version = word.to_string();
                continue;
            }
            if !version.is_empty() {
                if revision.is_empty() && word.starts_with('(') && word.ends_with(')') {
                    revision = word[1..word.len() - 1].to_string();
                }
                // Everything after the version that is not the revision
                // is the model list, which has no column.
                continue;
            }
            name_words.push(word);
        }
        (name_words.join(" "), version, revision)
    }

    /// What the image on a ROM row was identified as, for its [`RowKind::Note`]
    /// line. `None` for an image no checksum names, and for every other field.
    pub fn rom_note(&self, field: LauncherField) -> Option<&str> {
        let slot = match field {
            F::Rom => 0,
            F::ExtendedRom => 1,
            _ => return None,
        };
        self.rom_notes[slot].text.as_deref()
    }

    /// Stand in for a recognised image, so the ROM tab can be drawn and
    /// tested without a real Kickstart on the host.
    #[cfg(test)]
    pub fn set_rom_note_for_test(&mut self, field: LauncherField, text: &str) {
        let slot = match field {
            F::ExtendedRom => 1,
            _ => 0,
        };
        self.rom_notes[slot].path = self.setup.path(field).map(Path::to_path_buf);
        self.rom_notes[slot].text = Some(text.to_string());
        self.rom_notes[slot].cells = Self::split_identification(text);
        self.rom_notes[slot].seeded = true;
    }

    /// The text field currently being edited, if any.
    pub fn editing(&self) -> Option<EditTarget> {
        self.editing
    }

    /// The current edit buffer (for drawing the focused field).
    pub fn edit_buffer(&self) -> &str {
        &self.edit_buffer
    }

    /// Focus a board option for text entry, seeding the buffer with its value.
    pub fn begin_edit_board(&mut self, board: usize, opt: usize) {
        self.edit_buffer = self
            .setup
            .zorro_boards()
            .get(board)
            .map(|b| b.value(opt))
            .unwrap_or_default();
        self.editing = Some(EditTarget::BoardOption { board, opt });
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    /// Focus a drive's volume-name field, seeding the buffer with its value.
    pub fn begin_edit_drive_name(&mut self, field: LauncherField) {
        self.edit_buffer = self.setup.drive_name(field).unwrap_or_default().to_string();
        self.editing = Some(EditTarget::DriveName(field));
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    /// Focus a drive's boot-priority field for typing, seeding the buffer with
    /// the current number (a blank commit returns it to the unset default).
    pub fn begin_edit_drive_bootpri(&mut self, field: LauncherField) {
        // Do not offer typing on a greyed row (no image, a CD image, or a
        // drive whose Bootable box is cleared).
        if !self.setup.boot_field_applies(field) || self.setup.drive_boot_off(field) {
            return;
        }
        self.edit_buffer = match self.setup.drive_bootpri(field) {
            Some(n) => n.to_string(),
            None => String::new(),
        };
        self.editing = Some(EditTarget::DriveBootpri(field));
        self.edit_caret = Caret::end_of(&self.edit_buffer);
        self.status = None;
    }

    pub fn edit_push(&mut self, c: char) {
        let Some(target) = self.editing else { return };
        // The size is a whole number of MB or GB: digits only, and no more
        // than the box accepts.
        if let EditTarget::NewImageText(field) = target {
            if let Some(limit) = workshop_digit_limit(field) {
                // A boot priority is the one signed figure here.
                let minus_ok = field == F::NewHardBootPri
                    && c == '-'
                    && self.edit_caret.at() == 0
                    && !self.edit_buffer.starts_with('-');
                if (!c.is_ascii_digit() && !minus_ok) || self.edit_buffer.len() >= limit {
                    return;
                }
                self.edit_caret.insert(&mut self.edit_buffer, c);
                return;
            }
            if let Some(limit) = workshop_text_limit(field) {
                // A drive identity is read back by tools that expect the
                // plain printable ASCII a SCSI INQUIRY carries, so nothing
                // else gets in -- and never more than the field holds.
                if !c.is_ascii_graphic() && c != ' ' {
                    return;
                }
                if self.edit_buffer.chars().count() >= limit {
                    return;
                }
                self.edit_caret.insert(&mut self.edit_buffer, c);
                return;
            }
        }
        // An address is a run of printable characters with no spaces in it:
        // a name or a numeric literal, a colon, and a port. Brackets and
        // colons are in there for IPv6, so nothing narrower than "graphic"
        // would do.
        if let EditTarget::SerialAddr(_) = target {
            if !c.is_ascii_graphic() || self.edit_buffer.chars().count() >= SERIAL_ADDR_MAX {
                return;
            }
        }
        // A fixed fill is a 16-bit word. The parser accepts decimal too, so
        // admit decimal digits and the hexadecimal prefix/digits while keeping
        // the buffer no wider than `0xFFFF`.
        if target == EditTarget::RamPattern
            && (self.edit_buffer.chars().count() >= 6
                || !(c.is_ascii_hexdigit() || matches!(c, 'x' | 'X')))
        {
            return;
        }
        // A boot priority is a signed integer: digits, and a leading minus.
        if let EditTarget::DriveBootpri(_) = target {
            let minus_ok =
                c == '-' && self.edit_caret.at() == 0 && !self.edit_buffer.starts_with('-');
            if !(c.is_ascii_digit() || minus_ok) {
                return;
            }
        }
        self.edit_caret.insert(&mut self.edit_buffer, c);
    }

    pub fn edit_backspace(&mut self) {
        if self.editing.is_some() {
            self.edit_caret.backspace(&mut self.edit_buffer);
        }
    }

    /// Delete forwards, from the character the caret is on.
    pub fn edit_delete(&mut self) {
        if self.editing.is_some() {
            self.edit_caret.delete(&mut self.edit_buffer);
        }
    }

    /// Step the caret through the box being typed in.
    pub fn edit_caret_move(&mut self, to: CaretMove) {
        if self.editing.is_none() {
            return;
        }
        match to {
            CaretMove::Left => self.edit_caret.left(),
            CaretMove::Right => self.edit_caret.right(&self.edit_buffer),
            CaretMove::Home => self.edit_caret.home(),
            CaretMove::End => self.edit_caret.end(&self.edit_buffer),
        }
    }

    /// Where the caret is in the box being typed in, for drawing it.
    pub fn edit_caret(&self) -> Caret {
        self.edit_caret
    }

    /// Commit the edit buffer to the focused field. A drive name that would
    /// not survive the config validator keeps the field focused, so the name
    /// can be fixed instead of failing later at save.
    pub fn edit_commit(&mut self) {
        let Some(target) = self.editing else { return };
        match target {
            EditTarget::DriveName(_) => {
                let name = self.edit_buffer.trim();
                let invalid = (!name.is_empty())
                    .then(|| crate::filesys::volume_name_error(name))
                    .flatten();
                if let Some(err) = invalid {
                    self.status = Some(StatusMessage::err(err));
                    return;
                }
            }
            EditTarget::NewImageText(field) if workshop_digit_limit(field).is_some() => {
                // Every one of these is a number with a floor, so an empty
                // or unreadable box returns to the floor rather than
                // refusing: there is no wrong value to complain about.
                let typed = self.edit_buffer.trim();
                let w = &mut self.workshop;
                match field {
                    F::NewHardSize => {
                        w.size = typed
                            .parse::<u32>()
                            .unwrap_or(0)
                            .clamp(1, NEW_HARD_SIZE_MAX)
                    }
                    F::NewHardBootPri => w.boot_pri = typed.parse::<i8>().unwrap_or(0),
                    F::NewGeomCylinders => {
                        w.custom_geometry.cylinders = typed
                            .parse::<u32>()
                            .unwrap_or(0)
                            .clamp(2, workshop_ceiling(field))
                    }
                    F::NewGeomSurfaces => {
                        w.custom_geometry.surfaces = typed
                            .parse::<u32>()
                            .unwrap_or(0)
                            .clamp(1, workshop_ceiling(field))
                    }
                    F::NewGeomSectors => {
                        w.custom_geometry.sectors = typed
                            .parse::<u32>()
                            .unwrap_or(0)
                            .clamp(1, workshop_ceiling(field))
                    }
                    F::NewGeomReserved => {
                        w.reserved = typed
                            .parse::<u32>()
                            .unwrap_or(0)
                            .clamp(crate::diskimage::RESERVED_BLOCKS, workshop_ceiling(field))
                    }
                    _ => {}
                }
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::NewImageText(field) if workshop_text_limit(field).is_some() => {
                // Not an AmigaDOS name: an identity field is a run of
                // printable bytes a tool prints back, so anything typed
                // into it is already valid by the time it lands. Emptying
                // one is a choice too -- the field goes to spaces.
                let text = self.edit_buffer.trim().to_string();
                self.workshop.set_identity_field(field, text);
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::NewImageText(field) => {
                let text = self.edit_buffer.trim();
                // A volume name is an AmigaDOS name and has to survive
                // being one; a device name is what the mount is called.
                if let Some(err) = crate::filesys::volume_name_error(text) {
                    self.status = Some(StatusMessage::err(err));
                    return;
                }
                let text = text.to_string();
                match field {
                    F::NewFloppyLabel => self.workshop.floppy_label = text,
                    F::NewHardLabel => self.workshop.hard_label = text,
                    F::NewHardDevice => self.workshop.device = text,
                    _ => {}
                }
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::DriveBootpri(field) => {
                // A bad number keeps the field focused so it can be fixed, the
                // same as a rejected drive name. Typing the -128 sentinel clears
                // the Bootable box rather than storing an out-of-range priority.
                match parse_drive_bootpri(&self.edit_buffer) {
                    Ok(Some(BOOT_PRI_NEVER)) => self.setup.set_drive_boot_off(field, true),
                    Ok(value) => {
                        self.setup.set_drive_boot_off(field, false);
                        self.setup.set_drive_bootpri(field, value);
                    }
                    Err(err) => {
                        self.status = Some(StatusMessage::err(err.to_string()));
                        return;
                    }
                }
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::SerialAddr(field) => {
                // An emptied box means "unset": the dial-out address goes
                // away, and the listen address returns to the default. A
                // typed one has to be a host:port or it keeps the focus,
                // the same as a rejected drive name -- the mode is bound to
                // fail at Run otherwise, and this is where it can be fixed.
                let typed = self.edit_buffer.trim();
                let addr = if typed.is_empty() {
                    None
                } else {
                    if let Some(err) = serial_addr_error(typed) {
                        self.status = Some(StatusMessage::err(err));
                        return;
                    }
                    Some(typed.to_string())
                };
                #[cfg(feature = "midi")]
                self.setup.set_serial_addr(field, addr);
                #[cfg(not(feature = "midi"))]
                let _ = (field, addr);
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::RamPattern => {
                match crate::config::parse_ram_pattern(&self.edit_buffer) {
                    Ok(word) => self.setup.set_ram_pattern(word),
                    Err(err) => {
                        self.status = Some(StatusMessage::err(err.to_string()));
                        return;
                    }
                }
                self.editing = None;
                self.edit_buffer.clear();
                return;
            }
            EditTarget::BoardOption { .. } => {}
        }
        self.editing = None;
        let value = std::mem::take(&mut self.edit_buffer);
        match target {
            EditTarget::BoardOption { board, opt } => {
                self.setup.zorro_option_set(board, opt, value)
            }
            EditTarget::DriveName(field) => self.setup.set_drive_name(field, value),
            // These commit above and return, so nothing is left to do here.
            EditTarget::DriveBootpri(_)
            | EditTarget::NewImageText(_)
            | EditTarget::SerialAddr(_)
            | EditTarget::RamPattern => {}
        }
    }

    pub fn edit_cancel(&mut self) {
        self.editing = None;
        self.edit_buffer.clear();
    }

    /// Open the configuration panel seeded from a raw config (the running
    /// machine, or the defaults). An invalid raw config falls back to the
    /// defaults rather than refusing to open.
    pub fn from_raw(raw: &RawConfig) -> Self {
        Self::new(MachineSetup::from_raw(raw).unwrap_or_default())
    }
}

// --- helpers --------------------------------------------------------------

fn cpu_is_32bit(cpu: CpuModel) -> bool {
    matches!(
        cpu,
        CpuModel::M68020 | CpuModel::M68030 | CpuModel::M68040 | CpuModel::M68060
    )
}

/// Whether `field` appears anywhere with the given row kind. Used to classify a
/// field (toggle vs path) without threading the tab through every call, called
/// per drawn row, so it scans the static row tables directly rather than
/// composing tabs (which would allocate every frame). The composed tabs only
/// add `SectionHeader`/`BootpriHeader` rows, which carry no real field, so the
/// raw tables cover every classifiable field.
fn rows_contains_kind(field: LauncherField, kind: RowKind) -> bool {
    #[cfg(all(feature = "midi", feature = "mt32", feature = "coppersynth"))]
    let serial: &[&[Row]] = &[
        &SERIAL_ROWS_MIDI,
        &SERIAL_ROWS_MT32,
        &SERIAL_ROWS_CSYNTH,
        &SERIAL_ROWS_TCP_CONNECT,
        &SERIAL_ROWS_TCP_LISTEN,
    ];
    #[cfg(all(feature = "midi", feature = "mt32", not(feature = "coppersynth")))]
    let serial: &[&[Row]] = &[
        &SERIAL_ROWS_MIDI,
        &SERIAL_ROWS_MT32,
        &SERIAL_ROWS_TCP_CONNECT,
        &SERIAL_ROWS_TCP_LISTEN,
    ];
    #[cfg(all(feature = "midi", not(feature = "mt32"), feature = "coppersynth"))]
    let serial: &[&[Row]] = &[
        &SERIAL_ROWS_MIDI,
        &SERIAL_ROWS_CSYNTH,
        &SERIAL_ROWS_TCP_CONNECT,
        &SERIAL_ROWS_TCP_LISTEN,
    ];
    #[cfg(all(feature = "midi", not(feature = "mt32"), not(feature = "coppersynth")))]
    let serial: &[&[Row]] = &[
        &SERIAL_ROWS_MIDI,
        &SERIAL_ROWS_TCP_CONNECT,
        &SERIAL_ROWS_TCP_LISTEN,
    ];
    #[cfg(not(feature = "midi"))]
    let serial: &[&[Row]] = &[];
    let sources: &[&[Row]] = &[
        &SYSTEM_ROWS,
        &CPU_ROWS,
        &MEMORY_ROWS,
        &ROM_ROWS,
        &FLOPPY_ROWS,
        &STORAGE_ROWS,
        &HOSTFS_ROWS,
        &WHDLOAD_ROWS,
        &CD_ROWS,
        &LIDE_ROWS,
        &INPUT_ROWS,
        &VIDEO_ROWS,
        &AUDIO_ROWS,
        &EMULATION_ROWS,
        &PARALLEL_ROWS_PRINTER,
        &PARALLEL_ROWS_SAMPLER,
    ];
    sources
        .iter()
        .chain(serial.iter())
        .flat_map(|table| table.iter())
        .any(|r| r.field == field && r.kind == kind)
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build a `[ide]`/`[scsi]` drive entry from an editable path + optional
/// volume-name override. A blank name emits the bare path string so saved
/// configs stay minimal.
fn drive_raw(
    path: Option<&Path>,
    name: Option<&str>,
    bootpri: Option<i8>,
    filesystem: crate::diskimage::FileSystem,
) -> Option<RawDrive> {
    path.map(|p| RawDrive {
        path: path_string(p),
        name: name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        bootpri,
        // OFS is only meaningful (and only valid, config-side) on a
        // directory mount; FFS is the default, so it too stays unwritten,
        // keeping a saved config minimal exactly like `name` above.
        filesystem: (p.is_dir() && !filesystem.ffs).then(|| "ofs".to_string()),
    })
}

/// A `[scsi]` unit entry by SCSI ID. `RawScsi` names its units as separate
/// fields (that is the TOML shape), so indexed access needs this shim.
fn raw_scsi_unit(scsi: &crate::config::RawScsi, unit: usize) -> Option<&RawDrive> {
    match unit {
        0 => scsi.unit0.as_ref(),
        1 => scsi.unit1.as_ref(),
        2 => scsi.unit2.as_ref(),
        3 => scsi.unit3.as_ref(),
        4 => scsi.unit4.as_ref(),
        5 => scsi.unit5.as_ref(),
        _ => scsi.unit6.as_ref(),
    }
}

/// Boot priorities offered on the Host FS boot-pri stepper. -128 is the
/// "never boot" sentinel; the rest bracket the usual device priorities
/// (hard-disk partitions boot at 0, DF0: at 5).
const BOOTPRI_STEPS: [i8; 8] = [-128, -10, -5, 0, 5, 6, 10, 20];

/// Display form of a hard-disk boot priority in the Priority column: the number,
/// with unset shown as 0 (its effective value). A cleared Bootable box arrives
/// as the -128 "never" sentinel and displays as -128.
fn drive_bootpri_label(pri: Option<i8>) -> String {
    pri.unwrap_or(0).to_string()
}

/// Lowest priority the arrows/typing may reach while Bootable is ticked; -128 is
/// reserved for the cleared box, so an enabled priority stops at -127.
const BOOT_PRI_MIN_ENABLED: i8 = BOOT_PRI_NEVER + 1;

/// Nudge a hard-disk boot priority by one, clamped to -127..=127 (the -128
/// sentinel is the Bootable box, not a step). Unset steps off from 0.
fn step_drive_bootpri(current: Option<i8>, forward: bool) -> Option<i8> {
    let base = current.unwrap_or(0);
    let stepped = if forward {
        base.saturating_add(1)
    } else {
        base.saturating_sub(1)
    };
    Some(stepped.clamp(BOOT_PRI_MIN_ENABLED, i8::MAX))
}

/// The cascade default for the `rank`-th present hard-disk drive when the config
/// set no priority: the first is 0 (a strong boot candidate, just under DF0's
/// 5), and every later drive drops below all four floppies -- -35, -40, -45...
/// `None` (rank 0) leaves the key unwritten; the rest are explicit.
fn hdd_boot_cascade(rank: usize) -> Option<i8> {
    (rank > 0).then(|| (-30 - 5 * rank as i32).max(i8::MIN as i32) as i8)
}

/// Split a config `bootpri` into the launcher's two slots: the -128 "never"
/// sentinel clears the Bootable box (priority unset), any other value is the
/// priority, and a missing key is unset.
fn boot_priority_of(raw: Option<i8>) -> Option<i8> {
    raw.filter(|&p| p != BOOT_PRI_NEVER)
}

fn boot_is_off(raw: Option<i8>) -> bool {
    raw == Some(BOOT_PRI_NEVER)
}

/// SCSI unit index behind a `ScsiUnitN`/`ScsiUnitNBoot` field.
fn scsi_boot_index(field: LauncherField) -> Option<usize> {
    use LauncherField as F;
    Some(match field {
        F::ScsiUnit0 | F::ScsiUnit0Boot => 0,
        F::ScsiUnit1 | F::ScsiUnit1Boot => 1,
        F::ScsiUnit2 | F::ScsiUnit2Boot => 2,
        F::ScsiUnit3 | F::ScsiUnit3Boot => 3,
        F::ScsiUnit4 | F::ScsiUnit4Boot => 4,
        F::ScsiUnit5 | F::ScsiUnit5Boot => 5,
        F::ScsiUnit6 | F::ScsiUnit6Boot => 6,
        _ => return None,
    })
}

/// Lide drive index behind a `LideDriveN`/`LideDriveNBoot` field.
fn lide_drive_index(field: LauncherField) -> Option<usize> {
    use LauncherField as F;
    Some(match field {
        F::LideDrive0 | F::LideDrive0Boot => 0,
        F::LideDrive1 | F::LideDrive1Boot => 1,
        F::LideDrive2 | F::LideDrive2Boot => 2,
        F::LideDrive3 | F::LideDrive3Boot => 3,
        _ => return None,
    })
}

/// The boot-priority field for a hard-disk drive field (inverse of
/// [`MachineSetup::boot_field_drive`]).
fn drive_boot_field(drive: LauncherField) -> Option<LauncherField> {
    use LauncherField as F;
    Some(match drive {
        F::IdeMaster => F::IdeMasterBoot,
        F::IdeSlave => F::IdeSlaveBoot,
        F::ScsiUnit0 => F::ScsiUnit0Boot,
        F::ScsiUnit1 => F::ScsiUnit1Boot,
        F::ScsiUnit2 => F::ScsiUnit2Boot,
        F::ScsiUnit3 => F::ScsiUnit3Boot,
        F::ScsiUnit4 => F::ScsiUnit4Boot,
        F::ScsiUnit5 => F::ScsiUnit5Boot,
        F::ScsiUnit6 => F::ScsiUnit6Boot,
        F::LideDrive0 => F::LideDrive0Boot,
        F::LideDrive1 => F::LideDrive1Boot,
        F::LideDrive2 => F::LideDrive2Boot,
        F::LideDrive3 => F::LideDrive3Boot,
        _ => return None,
    })
}

/// Parse typed Boot Priority input: empty returns to the unset default (`None`),
/// any integer in -128..=127 becomes that value (-128 clears Bootable at the
/// call site), anything else is rejected.
fn parse_drive_bootpri(text: &str) -> std::result::Result<Option<i8>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<i8>()
        .map(Some)
        .map_err(|_| "boot priority must be -128..127")
}

fn cycle_bootpri(current: i8, forward: bool) -> i8 {
    let idx = BOOTPRI_STEPS
        .iter()
        .position(|&p| p == current)
        .unwrap_or_else(|| {
            // An off-list value (hand-edited config): snap to the nearest.
            BOOTPRI_STEPS
                .iter()
                .enumerate()
                .min_by_key(|(_, &p)| (i32::from(p) - i32::from(current)).abs())
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
    let n = BOOTPRI_STEPS.len();
    let next = if forward {
        (idx + 1) % n
    } else {
        (idx + n - 1) % n
    };
    BOOTPRI_STEPS[next]
}

/// The tail a MIDI picker's list carries when the built-in module belongs
/// on it: the module is not a host endpoint, so it is added rather than
/// enumerated. Nothing to add on a build without it.
#[cfg(feature = "midi")]
fn mt32_endpoint(wanted: bool) -> Option<String> {
    (wanted && cfg!(feature = "mt32")).then(|| crate::config::MIDI_OUT_MT32.to_string())
}

fn csynth_endpoint(wanted: bool) -> Option<String> {
    (wanted && cfg!(feature = "coppersynth")).then(|| crate::config::MIDI_OUT_CSYNTH.to_string())
}

fn cycle_slice<T: Copy + PartialEq>(items: &[T], current: T, forward: bool) -> T {
    let n = items.len();
    let idx = items.iter().position(|&x| x == current).unwrap_or(0);
    let next = if forward {
        (idx + 1) % n
    } else {
        (idx + n - 1) % n
    };
    items[next]
}

/// Cycle a network board's fitted/backend state (`None` = not fitted)
/// through the backends this build can actually bring up: NAT only where
/// available, a bridge choice only when an adapter exists to name (keeping
/// the current adapter if the board is already bridged). Shared by the
/// A2065 and HostSocket pickers in the Ethernet section.
fn cycle_net_board(
    net: &mut Option<NetConfig>,
    bridge_interfaces: &[(String, String)],
    forward: bool,
) {
    let mut choices = vec![None, Some(NetConfig::None), Some(NetConfig::Loopback)];
    if crate::net::NAT_AVAILABLE {
        choices.push(Some(NetConfig::Nat));
    }
    if crate::net::BRIDGE_AVAILABLE {
        let interface = match net.as_ref() {
            Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
            _ => bridge_interfaces.first().map(|(name, _)| name.clone()),
        };
        if let Some(interface) = interface {
            choices.push(Some(NetConfig::Bridge { interface }));
        }
    }
    let index = choices.iter().position(|item| item == net).unwrap_or(0);
    let next = if forward {
        (index + 1) % choices.len()
    } else {
        (index + choices.len() - 1) % choices.len()
    };
    *net = choices[next].clone();
}

/// `cycle_net_board`'s HostSocket-only counterpart: same choices, plus one
/// more this board alone supports -- `Host`, real host OS sockets via
/// direct `bsdsocket.library` passthrough (`crate::hostsocket`'s own doc
/// comment). Not a `NetConfig` backend at all (the A2065 is a real
/// Ethernet card; it has no such passthrough to offer), so it can't be a
/// `cycle_net_board` choice -- represented here by `host_mode`, a flag
/// alongside `net` rather than a `NetConfig` variant of its own. `net` is
/// irrelevant once `host_mode` is set (`Config::from_raw` always resolves
/// `net = "host"` to a `Loopback` smoltcp backend underneath regardless of
/// what the launcher's own `net` field holds -- see `to_raw`'s own
/// `hostsocket_host_mode` branch, which ignores it entirely when saving),
/// so `Host` is simply appended as one final `(None, true)` step before
/// wrapping back to plain `None`.
fn cycle_hostsocket_board(
    net: &mut Option<NetConfig>,
    host_mode: &mut bool,
    bridge_interfaces: &[(String, String)],
    forward: bool,
) {
    let mut choices: Vec<(Option<NetConfig>, bool)> = vec![
        (None, false),
        (Some(NetConfig::None), false),
        (Some(NetConfig::Loopback), false),
    ];
    if crate::net::NAT_AVAILABLE {
        choices.push((Some(NetConfig::Nat), false));
    }
    if crate::net::BRIDGE_AVAILABLE {
        let interface = match net.as_ref() {
            Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
            _ => bridge_interfaces.first().map(|(name, _)| name.clone()),
        };
        if let Some(interface) = interface {
            choices.push((Some(NetConfig::Bridge { interface }), false));
        }
    }
    choices.push((None, true));
    let current = if *host_mode {
        (None, true)
    } else {
        (net.clone(), false)
    };
    let index = choices
        .iter()
        .position(|item| item == &current)
        .unwrap_or(0);
    let next = if forward {
        (index + 1) % choices.len()
    } else {
        (index + choices.len() - 1) % choices.len()
    };
    let (chosen_net, chosen_host_mode) = choices[next].clone();
    *net = chosen_net;
    *host_mode = chosen_host_mode;
}

/// Cycle a bridged network board's host adapter through the visible ones;
/// inert unless the board's backend is currently a bridge.
fn cycle_bridge_interface(
    net: &mut Option<NetConfig>,
    bridge_interfaces: &[(String, String)],
    forward: bool,
) {
    if let Some(NetConfig::Bridge { interface }) = net.as_mut() {
        if !bridge_interfaces.is_empty() {
            let index = bridge_interfaces
                .iter()
                .position(|(name, _)| name == interface)
                .unwrap_or(0);
            let next = if forward {
                (index + 1) % bridge_interfaces.len()
            } else {
                (index + bridge_interfaces.len() - 1) % bridge_interfaces.len()
            };
            *interface = bridge_interfaces[next].0.clone();
        }
    }
}

/// Cycle through float presets, snapping to the nearest preset first so a
/// loaded off-grid value still steps sensibly.
fn cycle_floats(items: &[f64], current: f64, forward: bool) -> f64 {
    let idx = nearest_index_f64(items, current);
    let n = items.len();
    let next = if forward {
        (idx + 1) % n
    } else {
        (idx + n - 1) % n
    };
    items[next]
}

/// Cycle through `usize` size presets, snapping a loaded off-grid value to the
/// nearest preset before stepping.
fn cycle_nearest(items: &[usize], current: usize, forward: bool) -> usize {
    let idx = items
        .iter()
        .enumerate()
        .min_by_key(|(_, &v)| v.abs_diff(current))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let n = items.len();
    let next = if forward {
        (idx + 1) % n
    } else {
        (idx + n - 1) % n
    };
    items[next]
}

fn nearest_index_f64(items: &[f64], value: f64) -> usize {
    items
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (**a - value)
                .abs()
                .partial_cmp(&(**b - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

fn step_u8(current: u8, forward: bool, min: u8, max: u8) -> u8 {
    if forward {
        current.saturating_add(1).min(max)
    } else {
        current.saturating_sub(1).max(min)
    }
}

fn format_mhz(mhz: f64) -> String {
    if mhz.fract().abs() < 1e-6 {
        format!("{mhz:.0} MHz")
    } else {
        format!("{mhz:.2} MHz")
    }
}

fn size_label(bytes: usize) -> String {
    if bytes == 0 {
        "None".to_string()
    } else {
        format_size(bytes)
    }
}

// Names that round-trip through the config parsers (used by to_raw).

fn model_name(model: MachineModel) -> &'static str {
    match model {
        MachineModel::A500 => "A500",
        MachineModel::A500Ocs => "A500OCS",
        MachineModel::A500Plus => "A500Plus",
        MachineModel::A600 => "A600",
        MachineModel::A1200 => "A1200",
        MachineModel::A3000 => "A3000",
        MachineModel::A4000 => "A4000",
        MachineModel::Cdtv => "CDTV",
        MachineModel::Cd32 => "CD32",
        MachineModel::A1000 => "A1000",
    }
}

/// Friendlier label for the model selector buttons.
pub fn model_label(model: MachineModel) -> &'static str {
    match model {
        MachineModel::A500 => "A500",
        MachineModel::A500Ocs => "A500 OCS",
        MachineModel::A500Plus => "A500+",
        MachineModel::A600 => "A600",
        MachineModel::A1200 => "A1200",
        MachineModel::A3000 => "A3000",
        MachineModel::A4000 => "A4000",
        MachineModel::Cdtv => "CDTV",
        MachineModel::Cd32 => "CD32",
        MachineModel::A1000 => "A1000",
    }
}

fn rtg_card_name(card: RtgCard) -> &'static str {
    match card {
        RtgCard::None => "None",
        RtgCard::Picasso2 => "Picasso II",
        RtgCard::Picasso2Plus => "Picasso II+",
        RtgCard::Z3660 => "Z3660",
        RtgCard::GraffityZ2 => "Graffity Z2",
        RtgCard::GraffityZ3 => "Graffity Z3",
    }
}

fn rtg_card_value(card: RtgCard) -> &'static str {
    match card {
        RtgCard::None => "none",
        RtgCard::Picasso2 => "picasso2",
        RtgCard::Picasso2Plus => "picasso2plus",
        RtgCard::Z3660 => "z3660",
        RtgCard::GraffityZ2 => "graffityz2",
        RtgCard::GraffityZ3 => "graffityz3",
    }
}

fn chipset_name(chipset: Chipset) -> &'static str {
    match chipset {
        Chipset::Ocs => "OCS",
        Chipset::Ecs => "ECS",
        Chipset::Aga => "AGA",
    }
}

fn cpu_name(cpu: CpuModel) -> &'static str {
    match cpu {
        CpuModel::M68000 => "68000",
        CpuModel::M68010 => "68010",
        CpuModel::M68EC020 => "68EC020",
        CpuModel::M68020 => "68020",
        CpuModel::M68030 => "68030",
        CpuModel::M68040 => "68040",
        CpuModel::M68060 => "68060",
    }
}

fn agnus_name(agnus: AgnusRevision) -> &'static str {
    match agnus {
        AgnusRevision::Ocs => "OCS",
        AgnusRevision::Ecs8372Rev4 => "8372A",
        AgnusRevision::Ecs8375 => "8375",
        AgnusRevision::AgaAlice => "ALICE",
    }
}

fn denise_name(denise: DeniseRevision) -> &'static str {
    match denise {
        DeniseRevision::Ocs => "OCS",
        DeniseRevision::Ecs8373 => "ECS",
        DeniseRevision::AgaLisa => "LISA",
    }
}

fn video_name(video: VideoStandard) -> &'static str {
    match video {
        VideoStandard::Pal => "PAL",
        VideoStandard::Ntsc => "NTSC",
    }
}

fn overscan_name(overscan: Overscan) -> &'static str {
    match overscan {
        Overscan::Tv => "tv",
        Overscan::Full => "full",
    }
}

fn pixel_aspect_name(aspect: PixelAspect) -> &'static str {
    match aspect {
        PixelAspect::Tv => "tv",
        PixelAspect::Square => "square",
    }
}

fn display_scaling_name(scaling: DisplayScaling) -> &'static str {
    match scaling {
        DisplayScaling::Smooth => "smooth",
        DisplayScaling::Integer => "integer",
    }
}

/// The `[display] shader` value for a mode: the canonical preset name (not
/// the picker's "off" spelling, which only parses back), or a user shader's
/// path as written.
fn shader_name(shader: &ShaderMode) -> String {
    match shader {
        ShaderMode::None => "none".to_string(),
        ShaderMode::Scanlines => "scanlines".to_string(),
        ShaderMode::Mask => "mask".to_string(),
        ShaderMode::Crt => "crt".to_string(),
        ShaderMode::Custom(path) => path_string(path),
    }
}

/// The `[display] tint` value for a tint: the canonical config name (not
/// the picker's "off" spelling, which only parses back).
fn tint_name(tint: Tint) -> &'static str {
    match tint {
        Tint::None => "none",
        Tint::Bw => "bw",
        Tint::Green => "green",
        Tint::Amber => "amber",
        Tint::Sepia => "sepia",
    }
}

fn pacing_name(pacing: PacingBudget) -> &'static str {
    match pacing {
        PacingBudget::Cycles => "cycles",
        PacingBudget::Instructions => "instructions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_initialisation_controls_cycle_and_round_trip() {
        let raw: RawConfig = toml::from_str("[memory]\ninit = \"random:0xBEEF\"\n").unwrap();
        let mut setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(setup.value_label(F::RamInit), "Random");
        assert_eq!(
            setup.to_raw().memory.init.as_deref(),
            Some("random:0x000000000000BEEF")
        );

        setup.cycle(F::RamInit, true);
        assert_eq!(setup.value_label(F::RamInit), "Fixed");
        assert_eq!(setup.value_label(F::RamPattern), "0x5555");
        assert!(setup.applies(F::RamPattern));
        setup.set_ram_pattern(0xDEAD);
        assert_eq!(
            setup.to_raw().memory.init.as_deref(),
            Some("pattern:0xDEAD")
        );

        setup.cycle(F::RamInit, true);
        assert_eq!(setup.value_label(F::RamInit), "Zero");
        // The pattern row disappears rather than greying: it only
        // exists while the fill is Fixed.
        assert!(setup.row_hidden(F::RamPattern));
        setup.cycle(F::RamInit, false);
        assert!(!setup.row_hidden(F::RamPattern));
        assert_eq!(setup.value_label(F::RamPattern), "0xDEAD");

        setup.cycle(F::RamInit, false);
        assert_eq!(setup.value_label(F::RamInit), "Random");
        assert_eq!(
            setup.to_raw().memory.init.as_deref(),
            Some("random:0x000000000000BEEF")
        );
    }

    #[test]
    fn fixed_ram_pattern_text_box_validates_and_commits() {
        let mut setup = MachineSetup::default();
        setup.cycle(F::RamInit, false); // Zero -> Fixed
        let mut state = LauncherState::new(setup);
        state.begin_edit_ram_pattern();
        assert_eq!(state.editing(), Some(EditTarget::RamPattern));
        assert_eq!(state.edit_buffer(), "0x5555");

        state.edit_buffer.clear();
        for c in "0x1234".chars() {
            state.edit_push(c);
        }
        state.edit_commit();
        assert_eq!(state.setup.value_label(F::RamPattern), "0x1234");
        assert_eq!(
            state.setup.to_raw().memory.init.as_deref(),
            Some("pattern:0x1234")
        );

        state.begin_edit_ram_pattern();
        state.edit_buffer = "0x10000".to_string();
        state.edit_caret = Caret::end_of(&state.edit_buffer);
        state.edit_commit();
        assert_eq!(state.editing(), Some(EditTarget::RamPattern));
        assert!(state
            .status
            .as_ref()
            .is_some_and(|s| s.kind == StatusKind::Error));
        assert_eq!(state.setup.value_label(F::RamPattern), "0x1234");
    }

    /// The page greys what an interface does not honour, exactly as the
    /// library's own driver table reports it: the launcher carries no driver
    /// knowledge of its own, so the expectation here is derived from the same
    /// table and covers whichever drivers this build compiled in. A driver
    /// the build does not carry deliberately greys nothing -- there is
    /// nothing to ask, and a dead page cannot be fixed from the page.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn bridge_rows_grey_what_the_interface_does_not_support() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.set_bridge_edit_drive(0);
        // Capability greying is only reachable with an interface attached and
        // selected, and with a port to pick; without either the page greys
        // first (see `bridge_page_greys_without_an_interface`). Pinned rather
        // than sampled: what a test machine has attached is not this test's
        // subject.
        setup.bridge_status = BridgeStatus::Attached;
        setup.df_bridge_none[0] = false;
        setup.bridge_ports = vec![None, Some("/dev/ttyACM0".to_string())];

        let offered = bridge_drivers();
        assert!(!offered.is_empty(), "the bridge offers its drivers");
        for driver in offered {
            let info = crate::fluxbridge::driver_named(driver.match_token())
                .expect("offered drivers come from the library's table");
            let has_select = info.supports(crate::fluxbridge::config_option::DRIVE_AB_CABLE)
                || info.supports(crate::fluxbridge::config_option::SUPPORTS_SHUGART);
            setup.df_bridge[0].as_mut().expect("bridged").driver = driver;
            assert_eq!(
                setup.disabled_reason(F::BridgeCable).is_none(),
                has_select,
                "drive select on {driver:?}"
            );
            // Every interface here talks over a serial port.
            assert!(setup.disabled_reason(F::BridgePort).is_none());
        }
    }

    /// A disk chosen on the Host Disk page reaches the machine's
    /// configuration, and comes back the same when it is read again -- which
    /// is what makes an attachment outlive the session that made it.
    #[test]
    fn a_mounted_host_disk_round_trips_through_the_configuration() {
        let mut setup = MachineSetup::default();
        setup.set_host_disks_for_test(vec![
            HostDiskRow {
                id: "disk4".to_string(),
                fingerprint: None,
                volume: "SanDisk".to_string(),
                size: "31.9 GB".to_string(),
                mounted: Vec::new(),
                writable: true,
                attach: Some(crate::config::HostDiskAttach::IdeSlave),
            },
            HostDiskRow {
                id: "disk9".to_string(),
                fingerprint: None,
                volume: "Kingston".to_string(),
                size: "3.9 GB".to_string(),
                mounted: Vec::new(),
                writable: false,
                attach: Some(crate::config::HostDiskAttach::IdeMaster),
            },
        ]);

        setup.select_model(Some(MachineModel::A1200));
        // Ticking claims the first free attachment point, whatever the row
        // happened to be showing.
        setup.select_host_disk(0);
        assert_eq!(
            setup.host_disks()[0].attach,
            Some(crate::config::HostDiskAttach::IdeMaster)
        );
        // The second disk lands beside the first, not on it.
        setup.select_host_disk(1);
        assert_eq!(
            setup.host_disks()[1].attach,
            Some(crate::config::HostDiskAttach::IdeSlave)
        );

        let mounted = setup.mount_host_disks().expect("ticked disks mount");
        assert_eq!(mounted.len(), 2);
        assert_eq!(mounted[0].device, "disk4");
        assert_eq!(mounted[0].attach, crate::config::HostDiskAttach::IdeMaster);
        assert!(mounted[0].writable, "the choice to allow writing carries");
        assert!(!mounted[1].writable);
        // Mounting leaves the ticks alone: they show what the machine has, so
        // coming back to the page shows the machine rather than a blank slate.
        assert!(setup.host_disk_is_selected("disk4"));

        // Each shows on the row that holds it.
        assert_eq!(
            setup
                .host_disk_on_row(LauncherField::IdeMaster)
                .map(|d| d.device.as_str()),
            Some("disk4")
        );
        assert_eq!(
            setup
                .host_disk_on_row(LauncherField::IdeSlave)
                .map(|d| d.device.as_str()),
            Some("disk9")
        );

        // Out to a configuration file, and back.
        let raw = setup.to_raw();
        assert_eq!(raw.host_disk.len(), 2);
        assert_eq!(raw.host_disk[0].device, "disk4");
        assert_eq!(raw.host_disk[0].attach.as_deref(), Some("ide-master"));
        // Physical-disk access is always explicit in a saved configuration;
        // older entries with no field safely load read-only.
        assert_eq!(raw.host_disk[0].read_only, Some(false));
        assert_eq!(raw.host_disk[1].read_only, Some(true));

        let reloaded = MachineSetup::from_raw(&raw).expect("the written configuration reads back");
        let back = reloaded
            .host_disk_at(crate::config::HostDiskAttach::IdeMaster)
            .expect("the attachment survives being written and read");
        assert_eq!(back.device, "disk4");
        assert!(back.writable);

        // A machine with no IDE port refuses rather than configuring a disk
        // it could never reach, and says which port it wants.
        let mut no_ide = MachineSetup::default();
        no_ide.select_model(Some(MachineModel::A500));
        no_ide.set_host_disks_for_test(vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: None,
            volume: "SanDisk".to_string(),
            size: "31.9 GB".to_string(),
            mounted: Vec::new(),
            writable: false,
            attach: Some(crate::config::HostDiskAttach::IdeMaster),
        }]);
        no_ide.select_host_disk(0);
        assert!(
            !no_ide.host_disk_is_selected("disk4"),
            "there is nowhere on an A500 to put it"
        );
        assert_eq!(
            no_ide.host_disk_warning(),
            Some("Host disk attach requires an A600, A1200, A4000 or SCSI controller")
        );
        assert!(no_ide.mount_host_disks().is_err());
        assert!(no_ide.host_disks_attached().is_empty());

        // And taking it off gives it back to the host.
        let mut reloaded = reloaded;
        assert_eq!(
            reloaded.unmount_host_disk(crate::config::HostDiskAttach::IdeMaster),
            Some("disk4".to_string())
        );
        assert_eq!(reloaded.host_disks_attached().len(), 1);
        assert_eq!(reloaded.to_raw().host_disk.len(), 1);
    }

    #[test]
    fn a_fingerprinted_disk_survives_an_os_ordinal_change() {
        let mut setup = MachineSetup {
            host_disks_attached: vec![crate::config::HostDiskConfig {
                device: "disk4".to_string(),
                fingerprint: Some("v1-stable".to_string()),
                identity_confirmed: false,
                attach: crate::config::HostDiskAttach::IdeMaster,
                writable: false,
            }],
            host_disk_selected: vec!["disk4".to_string()],
            host_disks: vec![HostDiskRow {
                id: "disk9".to_string(),
                fingerprint: Some("v1-stable".to_string()),
                volume: "same medium".to_string(),
                size: "4.0 GB".to_string(),
                mounted: Vec::new(),
                writable: false,
                attach: None,
            }],
            ..Default::default()
        };

        setup.reconcile_host_disk_rows(&[]);

        assert_eq!(setup.host_disks_attached[0].device, "disk9");
        assert!(setup.host_disk_is_selected("disk9"));
        assert_eq!(
            setup.host_disks[0].attach,
            Some(crate::config::HostDiskAttach::IdeMaster)
        );
    }

    #[test]
    fn a_same_ordinal_replacement_inherits_no_disk_authority() {
        let mut setup = MachineSetup {
            host_disks_attached: vec![crate::config::HostDiskConfig {
                device: "disk4".to_string(),
                fingerprint: Some("v1-original".to_string()),
                identity_confirmed: true,
                attach: crate::config::HostDiskAttach::IdeMaster,
                writable: true,
            }],
            host_disk_selected: vec!["disk4".to_string()],
            ..Default::default()
        };
        let previous = vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: Some("v1-original".to_string()),
            volume: "original".to_string(),
            size: "4.0 GB".to_string(),
            mounted: Vec::new(),
            writable: true,
            attach: Some(crate::config::HostDiskAttach::IdeMaster),
        }];
        setup.host_disks = vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: Some("v1-replacement".to_string()),
            volume: "replacement".to_string(),
            size: "4.0 GB".to_string(),
            mounted: Vec::new(),
            writable: false,
            attach: None,
        }];

        setup.reconcile_host_disk_rows(&previous);

        assert!(!setup.host_disk_is_selected("disk4"));
        assert!(!setup.host_disks[0].writable);
        assert_eq!(setup.host_disks[0].attach, None);
        assert_eq!(setup.host_disks_attached[0].device, "disk4");
        assert_eq!(
            setup.host_disks_attached[0].fingerprint.as_deref(),
            Some("v1-original")
        );
    }

    /// A disk has a place only while it is ticked. Ticking assigns the first
    /// free point (IDE Master leads), unticking blanks it again, a choice
    /// stepped on a ticked row survives other disks coming and going, and
    /// when every place is taken the tick does not happen at all and says
    /// why.
    #[test]
    fn a_place_exists_only_while_the_disk_is_ticked() {
        use crate::config::HostDiskAttach as A;
        let disks = |n: usize| -> Vec<HostDiskRow> {
            (0..n)
                .map(|i| HostDiskRow {
                    id: format!("disk{i}"),
                    fingerprint: None,
                    volume: format!("Card {i}"),
                    size: "4.0 GB".to_string(),
                    mounted: Vec::new(),
                    writable: true,
                    attach: None,
                })
                .collect()
        };

        let mut setup = MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        setup.set_host_disks_for_test(disks(3));

        // Blank until ticked, and stepping a blank cell is not a request.
        assert_eq!(setup.host_disks()[0].attach, None);
        setup.cycle_host_disk_attach(0, true);
        assert_eq!(setup.host_disks()[0].attach, None);

        // Ticking assigns the first free point; the next tick the next.
        setup.select_host_disk(0);
        assert_eq!(setup.host_disks()[0].attach, Some(A::IdeMaster));
        setup.select_host_disk(1);
        assert_eq!(setup.host_disks()[1].attach, Some(A::IdeSlave));

        // A choice stepped on a ticked row stands while the disk stays
        // ticked, and unticking another disk frees its place.
        setup.select_host_disk(1);
        assert_eq!(setup.host_disks()[1].attach, None, "unticking blanks it");
        setup.cycle_host_disk_attach(0, true);
        assert_eq!(setup.host_disks()[0].attach, Some(A::IdeSlave));
        setup.select_host_disk(1);
        assert_eq!(
            setup.host_disks()[1].attach,
            Some(A::IdeMaster),
            "the freed place is picked up by the next tick"
        );

        // Nothing left on a machine with no SCSI: the third tick is refused,
        // stays unticked, and the next tick clears the warning.
        setup.select_host_disk(2);
        assert!(!setup.host_disk_is_selected("disk2"));
        assert_eq!(setup.host_disks()[2].attach, None);
        assert_eq!(
            setup.host_disk_warning(),
            Some("Every attachment point is already in use")
        );
        setup.select_host_disk(1);
        assert_eq!(setup.host_disk_warning(), None);
    }

    /// Taking the controller out takes its disks with it, rather than
    /// carrying them to Run to be refused there.
    #[test]
    fn a_disk_goes_when_the_port_it_was_on_does() {
        let mut setup = MachineSetup::default();
        setup.select_model(Some(MachineModel::A3000));
        setup.set_host_disks_for_test(vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: None,
            volume: "SanDisk".to_string(),
            size: "31.9 GB".to_string(),
            mounted: Vec::new(),
            writable: true,
            attach: None,
        }]);
        // Pick the motherboard controller, which is what makes its units real.
        while setup.scsi_controller_for_test() != Some(ScsiController::A3000) {
            setup.cycle(LauncherField::ScsiController, true);
        }
        setup.select_host_disk(0);
        assert_eq!(
            setup.host_disks()[0].attach,
            Some(crate::config::HostDiskAttach::Scsi(0)),
            "an A3000 has no IDE, so the first free point is a SCSI unit"
        );
        setup.mount_host_disks().expect("the A3000 has SCSI");
        assert_eq!(setup.host_disks_attached().len(), 1);

        // Take the controller away.
        while setup.scsi_controller_for_test().is_some() {
            setup.cycle(LauncherField::ScsiController, true);
        }
        assert!(!setup.has_scsi_controller());
        assert!(
            setup.host_disks_attached().is_empty(),
            "the disk goes with the controller"
        );
        assert!(!setup.host_disk_is_selected("disk4"));
        assert_eq!(
            setup.host_disks()[0].attach,
            None,
            "a disk with no port is going nowhere, and its cell reads blank"
        );
    }

    /// A real disk takes its place on the Boot Priority page like any other
    /// drive, but the priority itself is not ours to set: the partitions on
    /// the disk carry their own, and nothing here overrides them.
    #[test]
    fn a_host_disk_shows_on_boot_priority_with_its_priority_where_it_lives() {
        let mut setup = MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        setup.set_host_disks_for_test(vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: None,
            volume: "SanDisk".to_string(),
            size: "31.9 GB".to_string(),
            mounted: Vec::new(),
            writable: true,
            attach: Some(crate::config::HostDiskAttach::IdeMaster),
        }]);
        // Nothing attached: the row reads as an empty slot.
        assert_eq!(
            setup.disabled_reason(LauncherField::IdeMasterBoot),
            Some("No drive")
        );

        setup.select_host_disk(0);
        setup.mount_host_disks().expect("A1200 has an IDE port");
        assert_eq!(
            setup.disabled_reason(LauncherField::IdeMasterBoot),
            Some("Host Disk")
        );
        assert!(
            !setup.row_hidden(LauncherField::IdeMasterBoot),
            "the drive is there, so its row is too"
        );
    }

    /// The Interface row carries no driver list of its own: it offers what
    /// the library compiled in, in the library's order, so the row leads with
    /// the driver that build is meant to use. The starting value is pinned to
    /// "None" rather than sampled, because bridging a bay asks the host what
    /// is plugged in, and what a test machine has attached is not this test's
    /// subject.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn the_interface_row_offers_the_librarys_drivers_in_its_order() {
        let offered = bridge_drivers();
        let lead = *offered.first().expect("the bridge offers its drivers");
        let library: Vec<&str> = crate::fluxbridge::drivers()
            .iter()
            .map(|driver| driver.token)
            .collect();
        let row: Vec<&str> = offered.iter().map(|d| d.match_token()).collect();
        assert_eq!(row, library, "the row is the library's table, in order");

        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.set_bridge_edit_drive(0);
        setup.df_bridge_none[0] = true;
        assert_eq!(setup.value_label(F::BridgeDevice), "None");
        // One step forward off "None" reaches the first interface offered.
        setup.cycle(F::BridgeDevice, true);
        assert_eq!(setup.value_label(F::BridgeDevice), lead.label());
    }

    /// Drive speed acts on image bays only, so the row greys exactly when
    /// every fitted bay is physical: one image bay anywhere keeps it live.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn drive_speed_greys_when_every_fitted_bay_is_physical() {
        let mut setup = MachineSetup {
            floppy_drives: 2,
            ..Default::default()
        };
        assert_eq!(setup.disabled_reason(F::FloppySpeed), None);

        setup.set_drive_bridged(0, true);
        assert_eq!(
            setup.disabled_reason(F::FloppySpeed),
            None,
            "df1 is still an image bay"
        );

        setup.set_drive_bridged(1, true);
        assert!(
            setup.disabled_reason(F::FloppySpeed).is_some(),
            "every fitted bay is physical"
        );

        // An unfitted bay's state does not count: df2 is not wired in.
        setup.floppy_drives = 3;
        assert_eq!(
            setup.disabled_reason(F::FloppySpeed),
            None,
            "df2 is a fitted image bay again"
        );
    }

    /// The bridge page follows what is there: with nothing attached only the
    /// Interface row stays live, and with the bay pulled out from under the
    /// page (a loaded config can do that) every row greys, Interface included.
    /// With an interface attached the rows answer to the driver as before.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn bridge_page_greys_without_an_interface() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.set_bridge_edit_drive(0);
        // The port row answers to its own rules (tested below): it must stay
        // pickable for an interface the library's scan cannot name.
        let all = [
            F::BridgeCable,
            F::BridgeDensity,
            F::BridgeReadMode,
            F::BridgeReplaySpeed,
        ];

        setup.df_bridge_none[0] = false;
        setup.bridge_status = BridgeStatus::NoInterface;
        assert_eq!(setup.disabled_reason(F::BridgeDevice), None);
        for f in all {
            assert!(
                setup.disabled_reason(f).is_some(),
                "{f:?} live with no interface"
            );
        }

        setup.bridge_status = BridgeStatus::Attached;
        assert_eq!(setup.disabled_reason(F::BridgeDevice), None);
        for f in [F::BridgeDensity, F::BridgeReadMode, F::BridgeReplaySpeed] {
            assert_eq!(setup.disabled_reason(f), None, "{f:?} greyed with one");
        }

        // An interface of "None" greys the page whatever is attached, the
        // port row included.
        setup.df_bridge_none[0] = true;
        assert_eq!(setup.disabled_reason(F::BridgeDevice), None);
        for f in all {
            assert!(
                setup.disabled_reason(f).is_some(),
                "{f:?} live with interface None"
            );
        }
        assert!(setup.disabled_reason(F::BridgePort).is_some());
        setup.df_bridge_none[0] = false;

        // The port row greys exactly when there is nothing to point at:
        // a list of just "Automatic". Pinned, not sampled -- the machine
        // running this test has its own serial devices.
        setup.bridge_ports = vec![None];
        assert!(setup.disabled_reason(F::BridgePort).is_some());
        setup.bridge_ports = vec![None, Some("/dev/cu.wchusbserial1420".to_string())];
        assert_eq!(setup.disabled_reason(F::BridgePort), None);

        // The bay un-bridged underneath the page: nothing left to edit.
        setup.set_drive_bridged(0, false);
        assert!(setup.disabled_reason(F::BridgeDevice).is_some());
        for f in all {
            assert!(setup.disabled_reason(f).is_some(), "{f:?} live with no bay");
        }
    }

    /// Drive select is shaped by the interface, so it only answers to one
    /// when there is one: attached and chosen. That is what separates a row
    /// greyed with its steppers (this interface has no drive-select line)
    /// from one blanked entirely (there is no interface to ask).
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn drive_select_answers_to_an_interface_only_when_there_is_one() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.set_bridge_edit_drive(0);

        setup.bridge_status = BridgeStatus::NoInterface;
        setup.df_bridge_none[0] = false;
        assert!(!setup.bridge_interface_selected(), "nothing attached");

        setup.bridge_status = BridgeStatus::Attached;
        setup.df_bridge_none[0] = true;
        assert!(!setup.bridge_interface_selected(), "interface set to None");

        setup.df_bridge_none[0] = false;
        assert!(setup.bridge_interface_selected(), "attached and chosen");

        setup.set_drive_bridged(0, false);
        assert!(!setup.bridge_interface_selected(), "no bay at all");
    }

    /// A bridged bay only names its interface when one is actually attached:
    /// with nothing plugged in the media row says so, whatever the bay is
    /// configured for.
    // A build without the feature has no bridges to configure: the keys are
    // read and ignored, so there is nothing here to assert.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn a_bridged_bay_names_its_interface_only_when_one_is_attached() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.df_bridge_none[0] = false;

        setup.bridge_status = BridgeStatus::Attached;
        assert_eq!(
            setup.drive_bridge_label(0),
            crate::config::BridgeDriver::default().label()
        );

        setup.bridge_status = BridgeStatus::NoInterface;
        assert_eq!(setup.drive_bridge_label(0), "Not connected");

        // An interface of "None" reads the same: nothing is on the bay
        // either way.
        setup.bridge_status = BridgeStatus::Attached;
        setup.df_bridge_none[0] = true;
        assert_eq!(setup.drive_bridge_label(0), "Not connected");

        // An image-backed bay is not a bridge at all, connected or not.
        assert_eq!(setup.drive_bridge_label(1), "(none)");
    }

    /// An interface of "None" keeps the tick box and the page, but the built
    /// config -- what a run uses and what a save writes -- carries no bridge:
    /// the bay is effectively unbridged until an interface is chosen.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn a_none_interface_builds_an_unbridged_bay() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        setup.df_bridge_none[0] = true;
        let cfg = setup.build_config().expect("valid");
        assert!(cfg.floppy.bridges[0].is_none(), "no bridge in the config");

        setup.df_bridge_none[0] = false;
        let cfg = setup.build_config().expect("valid");
        assert!(
            cfg.floppy.bridges[0].is_some(),
            "an interface brings it back"
        );
    }

    /// "Automatic" leads, the library's scan keeps its order, and host
    /// devices join only when the scan did not already name them -- a macOS
    /// `cu.` device counts as named when its `tty.` twin is listed.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn port_lists_merge_without_duplicates() {
        let merged = merge_port_lists(
            vec!["/dev/tty.usbmodem1101".to_string()],
            vec![
                "/dev/cu.Bluetooth-Incoming-Port".to_string(),
                "/dev/cu.usbmodem1101".to_string(),
                "/dev/cu.wchusbserial1420".to_string(),
            ],
        );
        assert_eq!(
            merged,
            vec![
                None,
                Some("/dev/tty.usbmodem1101".to_string()),
                Some("/dev/cu.Bluetooth-Incoming-Port".to_string()),
                Some("/dev/cu.wchusbserial1420".to_string()),
            ]
        );
    }

    /// The write-protect box governs a real drive as well as an image, and it
    /// starts ticked: a bay handed a physical disk must not come up writable
    /// because nobody said otherwise.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn dropping_a_drive_releases_the_physical_one_it_was_holding() {
        let mut setup = MachineSetup {
            floppy_drives: 2,
            ..Default::default()
        };
        setup.set_drive_bridged(1, true);
        assert!(setup.df_bridge[1].is_some(), "df1 bridged");

        // Take the machine back to one drive. df1's row goes off the page, so
        // if it kept the bridge nothing would explain why the interface was
        // busy the next time a bay asked for it.
        setup.cycle(F::FloppyDrives, false);
        assert_eq!(setup.floppy_drives, 1);
        assert!(setup.df_bridge[1].is_none(), "df1 released the drive");

        // df0 is still fitted, so anything set on it stays put.
        setup.set_drive_bridged(0, true);
        setup.floppy_drives = 2;
        setup.cycle(F::FloppyDrives, false);
        assert!(setup.df_bridge[0].is_some(), "df0 kept its bridge");
    }

    // A build without the feature has no bridges to configure: the keys are
    // read and ignored, so there is nothing here to assert.
    #[cfg(feature = "fluxbridge")]
    #[test]
    fn write_protect_governs_a_bridged_bay_and_survives_a_round_trip() {
        let mut setup = MachineSetup::default();
        setup.set_drive_bridged(0, true);
        // Pin an interface: a hardware-less host auto-selects "None", which
        // deliberately keeps the bridge out of the built config.
        setup.df_bridge_none[0] = false;
        assert!(
            setup.toggle_value(F::Df0WriteProtect),
            "protected by default"
        );

        let cfg = setup.build_config().expect("valid");
        let bridge = cfg.floppy.bridges[0].as_ref().expect("bridged");
        assert!(bridge.write_protected);

        // Untick it, and both the emitted config and the bay's own copy follow.
        setup.toggle(F::Df0WriteProtect);
        assert!(!setup.toggle_value(F::Df0WriteProtect));
        assert!(
            !setup.df_bridge[0]
                .as_ref()
                .expect("bridged")
                .write_protected
        );
        let cfg = setup.build_config().expect("valid");
        assert!(
            !cfg.floppy.bridges[0]
                .as_ref()
                .expect("bridged")
                .write_protected
        );

        // And it comes back the same way, rather than reverting to protected.
        let reloaded = MachineSetup::from_raw(&setup.to_raw()).expect("round trip");
        assert!(!reloaded.toggle_value(F::Df0WriteProtect));
        assert!(
            !reloaded.df_bridge[0]
                .as_ref()
                .expect("bridged")
                .write_protected
        );
    }

    fn write_board_manifest() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "copperline-launcher-board-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"
            name = "Test Plugin"
            zorro = 2
            type = "wasm"
            size = "64K"
            manufacturer = 5192
            product = 16
            wasm = "x.wasm"
            [config]
            speed = "fast"
            verbose = false
            [[option]]
            key = "speed"
            label = "Speed"
            type = "enum"
            choices = ["slow", "fast"]
            [[option]]
            key = "verbose"
            label = "Verbose"
            type = "bool"
            [[option]]
            key = "count"
            label = "Count"
            type = "int"
            default = 3
            [[option]]
            key = "rom"
            label = "ROM"
            type = "file"
        "#,
        )
        .unwrap();
        path
    }

    #[test]
    fn plugin_board_options_load_edit_and_round_trip() {
        let path = write_board_manifest();
        let mut board = ZorroBoardSetup::load(path.clone());
        assert_eq!(board.options().len(), 4);
        // Defaults: [config] for speed/verbose, the option default for count.
        assert_eq!(board.value(0), "fast");
        assert_eq!(board.value(1), "false");
        assert_eq!(board.value(2), "3");
        assert_eq!(board.value(3), ""); // unset file

        board.cycle(0, true); // enum fast -> slow (wraps)
        assert_eq!(board.value(0), "slow");
        board.toggle(1); // bool false -> true
        assert_eq!(board.value(1), "true");
        board.cycle(2, false); // int 3 -> 2
        assert_eq!(board.value(2), "2");
        board.set(3, "/tmp/board.rom".into());
        assert_eq!(board.value(3), "/tmp/board.rom");
        board.clear(2); // revert int to its default
        assert_eq!(board.value(2), "3");

        // Overrides serialize back, typed per the option schema.
        let setup = MachineSetup {
            zorro_boards: vec![board],
            ..MachineSetup::default()
        };
        let raw = setup.to_raw();
        let cfg = raw.zorro[0].config.as_ref().expect("overrides emitted");
        assert_eq!(cfg.get("speed").unwrap().as_str(), Some("slow"));
        assert_eq!(cfg.get("verbose").unwrap().as_bool(), Some(true));
        assert_eq!(cfg.get("rom").unwrap().as_str(), Some("/tmp/board.rom"));
        // "count" was reverted to default, so it is not emitted.
        assert!(cfg.get("count").is_none());

        // And those overrides round-trip back through from_raw.
        let reloaded = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(reloaded.zorro_boards()[0].value(0), "slow");
        assert_eq!(reloaded.zorro_boards()[0].value(1), "true");

        let _ = std::fs::remove_file(&path);
    }

    fn raw_mount(path: &str) -> RawFilesysMount {
        RawFilesysMount {
            path: path.to_string(),
            volume: Some(path.trim_start_matches('/').to_uppercase()),
            bootpri: None,
            readonly: None,
        }
    }

    #[test]
    fn host_mounts_round_trip_and_keep_entries_past_the_gui_slots() {
        let mut raw = RawConfig {
            filesys: (0..6).map(|i| raw_mount(&format!("/host{i}"))).collect(),
            ..RawConfig::default()
        };
        // A hand-written readonly flag on a GUI-slot mount must survive a save.
        raw.filesys[0].readonly = Some(true);

        let mut setup = MachineSetup::from_raw(&raw).unwrap();
        // The GUI edits the first FILESYS_GUI_SLOTS mounts; the rest are held
        // verbatim so a save never drops a hand-written entry.
        assert_eq!(setup.filesys_dirs[0], Some(PathBuf::from("/host0")));
        assert_eq!(setup.filesys_dirs[3], Some(PathBuf::from("/host3")));
        assert_eq!(setup.filesys_extra.len(), 2);

        // An untouched save is a faithful round trip.
        assert_eq!(setup.to_raw().filesys, raw.filesys);

        // The Access spinner flips between the two modes; a writable mount
        // emits no readonly key at all rather than an explicit false.
        assert_eq!(
            setup.value_label(LauncherField::Filesys0ReadOnly),
            "Read-only"
        );
        setup.cycle(LauncherField::Filesys0ReadOnly, true);
        assert_eq!(
            setup.value_label(LauncherField::Filesys0ReadOnly),
            "Read-write"
        );
        assert_eq!(setup.to_raw().filesys[0].readonly, None);
        setup.cycle(LauncherField::Filesys0ReadOnly, false);
        assert_eq!(setup.to_raw().filesys[0].readonly, Some(true));

        // Clearing a slot removes that mount. HOSTFS<n> is the position in the
        // config, so the mounts after it renumber, exactly as they would if the
        // entry were deleted from the TOML by hand.
        setup.filesys_dirs[1] = None;
        setup.filesys_names[1] = None;
        let saved = setup.to_raw().filesys;
        let paths: Vec<&str> = saved.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["/host0", "/host2", "/host3", "/host4", "/host5"]);

        // The formerly-extra mounts now fall inside the GUI slots, and the
        // volume names still travel with their own paths.
        let back = MachineSetup::from_raw(&RawConfig {
            filesys: saved,
            ..RawConfig::default()
        })
        .unwrap();
        assert_eq!(back.filesys_dirs[1], Some(PathBuf::from("/host2")));
        assert_eq!(back.filesys_names[1].as_deref(), Some("HOST2"));
        assert_eq!(back.filesys_extra.len(), 1);
    }

    #[test]
    fn whdload_settings_round_trip_including_args() {
        // All four [whdload] keys, args included: args has no UI row, so it
        // must be carried through rather than edited.
        let raw = RawConfig {
            whdload: crate::config::RawWhdload {
                game: Some("/games/Turrican.lha".to_string()),
                library: Some("/whd/library".to_string()),
                kickstarts: Some("/roms/kickstarts".to_string()),
                args: Some("NoVBRMove ButtonWait".to_string()),
                ..crate::config::RawWhdload::default()
            },
            ..RawConfig::default()
        };
        let setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(
            setup.path(F::WhdloadGame),
            Some(Path::new("/games/Turrican.lha"))
        );
        assert_eq!(
            setup.path(F::WhdloadKickstarts),
            Some(Path::new("/roms/kickstarts"))
        );
        assert_eq!(
            setup.path(F::WhdloadLibrary),
            Some(Path::new("/whd/library"))
        );
        assert_eq!(setup.to_raw().whdload, raw.whdload);
    }

    /// Every Paths row must reach an entry, resolve to a directory, and
    /// come back out in `[paths]`. Three separate matches stand between a
    /// row and its entry -- one to read, one to write, one to resolve -- so
    /// this walks all of them for every row on the page: a row wired to the
    /// wrong entry, or to none, cannot pass.
    #[test]
    fn every_paths_row_reaches_its_own_entry() {
        // `set_path` on a Paths row adopts, which writes the process-wide
        // store another test asserts against.
        let _guard = crate::paths::adopted_store_lock();
        let mut setup = MachineSetup::default();
        let fields: Vec<LauncherField> = PATHS_ROWS
            .iter()
            .map(|row| row.field)
            .filter(|field| field.is_paths_field())
            .collect();
        assert_eq!(fields.len(), 11, "every entry should have had a row");
        for (i, &field) in fields.iter().enumerate() {
            assert!(
                !setup.paths_is_set(field),
                "{field:?} starts out set, so nothing here proves anything"
            );
            let dir = PathBuf::from(format!("/probe/{i}"));
            setup.set_path(field, dir.clone());
            assert_eq!(
                setup.path(field),
                Some(dir.as_path()),
                "{field:?} does not read back what it was given"
            );
            assert!(setup.paths_is_set(field), "{field:?} still reads inherited");
        }
        // Resolution is checked only once every row is set. Checked as each
        // one is set, a row resolving through a *later* row's entry would
        // still land on that row's untouched default and look fine.
        let mut seen: Vec<PathBuf> = Vec::new();
        for &field in &fields {
            let resolved = setup
                .paths_resolved(field)
                .expect("a Paths row resolves to a directory");
            assert!(
                !seen.contains(&resolved),
                "{field:?} resolves to {resolved:?}, which another row already claimed"
            );
            seen.push(resolved);
        }
        // Out through the configuration and back, since that is the only
        // way any of it survives.
        let round_tripped = MachineSetup::from_raw(&setup.to_raw()).expect("valid");
        for row in PATHS_ROWS {
            if row.field.is_paths_field() {
                assert_eq!(
                    round_tripped.path(row.field),
                    setup.path(row.field),
                    "{:?} did not survive [paths]",
                    row.field
                );
            }
        }
        // And cleared, each one goes back to inheriting rather than to a
        // frozen copy of today's default.
        for row in PATHS_ROWS {
            if row.field.is_paths_field() {
                setup.clear_path(row.field);
            }
        }
        assert!(
            setup.to_raw().paths.is_empty(),
            "a cleared page emits no [paths]"
        );
    }

    /// A machine configuration that never mentions directories must not
    /// grow a `[paths]` section just by passing through the launcher.
    #[test]
    fn an_untouched_paths_page_writes_nothing() {
        let raw = MachineSetup::default().to_raw();
        assert!(raw.paths.is_empty());
        let text = raw.to_toml_string().expect("serialises");
        assert!(!text.contains("[paths]"), "{text}");
    }

    #[test]
    fn whdload_paths_route_through_set_path_and_clear_path() {
        let mut setup = MachineSetup::default();
        for field in [F::WhdloadGame, F::WhdloadKickstarts, F::WhdloadLibrary] {
            assert_eq!(setup.path(field), None);
        }
        // Distinct paths per field, so cross-wired slots would show.
        setup.set_path(F::WhdloadGame, PathBuf::from("/g/game.lha"));
        setup.set_path(F::WhdloadKickstarts, PathBuf::from("/k"));
        setup.set_path(F::WhdloadLibrary, PathBuf::from("/l"));
        assert_eq!(setup.path(F::WhdloadGame), Some(Path::new("/g/game.lha")));
        assert_eq!(setup.path(F::WhdloadKickstarts), Some(Path::new("/k")));
        assert_eq!(setup.path(F::WhdloadLibrary), Some(Path::new("/l")));
        let raw = setup.to_raw();
        assert_eq!(raw.whdload.game.as_deref(), Some("/g/game.lha"));
        assert_eq!(raw.whdload.kickstarts.as_deref(), Some("/k"));
        assert_eq!(raw.whdload.library.as_deref(), Some("/l"));
        // The WHDLoad volumes have fixed names, so no path row here ever
        // grows a volume box -- an editable name with nothing to name.
        // Every one of them, so a row added later is not left with a box
        // that cannot be typed into.
        for field in [
            F::WhdloadGame,
            F::WhdloadGames,
            F::WhdloadKickstarts,
            F::WhdloadLibrary,
            F::WhdloadWhdPackage,
            F::WhdloadSkickPackage,
        ] {
            setup.set_path(field, PathBuf::from("/somewhere"));
            assert!(
                !setup.drive_name_applies(field),
                "{field:?} offers a volume box"
            );
        }
        for field in [
            F::WhdloadGame,
            F::WhdloadGames,
            F::WhdloadKickstarts,
            F::WhdloadLibrary,
            F::WhdloadWhdPackage,
            F::WhdloadSkickPackage,
        ] {
            setup.clear_path(field);
            assert_eq!(setup.path(field), None);
        }
        assert_eq!(setup.to_raw().whdload, crate::config::RawWhdload::default());
    }

    #[test]
    fn the_rom_tab_carries_an_identification_line_under_each_path_row() {
        // Each ROM path row is followed by a note row keyed on the same
        // field, so the identification is hidden and greyed with its row.
        let rows = rows(
            LauncherTab::Rom,
            ParallelDevice::None,
            SerialMode::Off,
            false,
            false,
        );
        let shape: Vec<(&str, RowKind, LauncherField)> =
            rows.iter().map(|r| (r.label, r.kind, r.field)).collect();
        assert_eq!(
            shape,
            [
                ("Primary ROM:", RowKind::SectionHeader, F::SectionHeader),
                ("  Kickstart ROM", RowKind::Path, F::Rom),
                ("", RowKind::RomInfoHeader, F::Rom),
                ("", RowKind::RomInfoValue, F::Rom),
                ("", RowKind::SectionHeader, F::SectionHeader),
                ("Extended ROM:", RowKind::SectionHeader, F::SectionHeader),
                ("  Extended ROM", RowKind::Path, F::ExtendedRom),
            ]
        );

        // The identification splits into the table's three columns; the
        // models after the revision have no column and are dropped.
        let mut probe = LauncherState::new(MachineSetup::default());
        probe.set_rom_note_for_test(F::Rom, "Kickstart 3.1 (40.68) A1200");
        assert_eq!(
            probe.rom_note_cells(F::Rom),
            (
                "Kickstart".to_string(),
                "3.1".to_string(),
                "40.68".to_string()
            )
        );
        // The bundled AROS pair fills both slots' tables with numbers
        // read off the images themselves, so they follow releases.
        let bundled = LauncherState::new(MachineSetup::default());
        let (name, version, revision) = bundled.rom_note_cells(F::Rom);
        assert_eq!(name, "AROS");
        assert!(
            !version.is_empty() && !revision.is_empty(),
            "the AROS image carries its own numbers"
        );
        // An unrecognised image is an empty table, not a missing one.
        let mut setup = MachineSetup::default();
        setup.set_path(F::Rom, std::path::PathBuf::from("mystery-dump.rom"));
        let unknown = LauncherState::new(setup);
        assert_eq!(
            unknown.rom_note_cells(F::Rom),
            (String::new(), String::new(), String::new())
        );

        let mut state = LauncherState::new(MachineSetup::default());
        // Nothing chosen: the value column says the machine boots the
        // bundled AROS, and there is no image to identify.
        assert_eq!(state.setup.value_label(F::Rom), "(bundled AROS)");
        assert_eq!(state.rom_note(F::Rom), None);
        assert_eq!(state.rom_note(F::ExtendedRom), None);

        // A file that is not a known ROM (and here does not exist at all)
        // leaves the line blank rather than claiming anything.
        state
            .setup
            .set_path(F::Rom, PathBuf::from("/roms/mystery.rom"));
        state.sync_rom_notes();
        assert_eq!(state.rom_note(F::Rom), None);

        // A recognised image: the note names the Kickstart, and the value
        // column still shows the file the user picked.
        state.set_rom_note_for_test(F::Rom, "Kickstart 3.1 (40.68) A1200");
        assert_eq!(state.setup.value_label(F::Rom), "mystery.rom");
        assert_eq!(state.rom_note(F::Rom), Some("Kickstart 3.1 (40.68) A1200"));
        // The identification is cached against the path: syncing again with
        // the same file in the field must not read (and so re-identify) it.
        state.sync_rom_notes();
        assert_eq!(state.rom_note(F::Rom), Some("Kickstart 3.1 (40.68) A1200"));
        // Choosing another file does re-identify, and clearing the row drops
        // the note with it.
        state
            .setup
            .set_path(F::Rom, PathBuf::from("/roms/other.rom"));
        state.sync_rom_notes();
        assert_eq!(state.rom_note(F::Rom), None);
        state.set_rom_note_for_test(F::Rom, "Kickstart 1.3 (34.5) A500/A1000/A2000");
        state.setup.clear_path(F::Rom);
        state.sync_rom_notes();
        assert_eq!(state.rom_note(F::Rom), None);
        // The extended ROM keeps its own line.
        assert_eq!(state.rom_note(F::ExtendedRom), None);
        state
            .setup
            .set_path(F::ExtendedRom, PathBuf::from("/roms/cd32-ext.rom"));
        state.set_rom_note_for_test(F::ExtendedRom, "CD32 extended ROM (40.60)");
        assert_eq!(state.rom_note(F::Rom), None);
        assert_eq!(
            state.rom_note(F::ExtendedRom),
            Some("CD32 extended ROM (40.60)")
        );
        // Only the ROM rows have one.
        assert_eq!(state.rom_note(F::Df0Image), None);
    }

    #[test]
    fn the_machine_type_cycle_reports_which_machine_it_means() {
        use crate::config::WhdloadMachine as M;
        let mut setup = MachineSetup::default();
        // Auto is the default: the slave header says what the game wants.
        assert_eq!(setup.whdload_machine(), M::Auto);
        assert_eq!(setup.value_label(F::WhdloadMachine), "Auto");

        // The two settings are one cycle apart either way round, so the
        // reading after a press is the reading the line describes.
        setup.cycle(F::WhdloadMachine, true);
        assert_eq!(setup.whdload_machine(), M::Copperline);
        assert_eq!(setup.value_label(F::WhdloadMachine), "Copperline");
        setup.cycle(F::WhdloadMachine, true);
        assert_eq!(setup.whdload_machine(), M::Auto);
        setup.cycle(F::WhdloadMachine, false);
        assert_eq!(setup.whdload_machine(), M::Copperline, "and backwards");
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn a_store_from_another_folder_is_not_this_folders_list() {
        use crate::gamelib::Known;
        // A store carrying a collection listed somewhere else -- which is
        // what a fresh configuration pointed at a new folder meets.
        let mut state = LauncherState::new(MachineSetup::default());
        state.library.db.set_known(
            (0..40)
                .map(|at| Known {
                    file: format!("Old{at}.lha"),
                    game: None,
                    manual: false,
                    slave_sha1: None,
                })
                .collect(),
        );
        state.library.db.set_folder(Path::new("/games/old"));
        state.library.db_loaded = true;

        // Pointed at a different folder, the page offers none of them: they
        // are paths under somewhere else and none of them is here.
        state
            .setup
            .set_path(F::WhdloadGames, PathBuf::from("/games/new"));
        state.refresh_library(Path::new("/nonexistent"));
        assert!(
            state.library.games.is_empty(),
            "another folder's games were listed as this one's"
        );

        // Once the store says it lists this folder, they are its list.
        state.library.db.set_folder(Path::new("/games/new"));
        state.refresh_library(Path::new("/nonexistent"));
        assert_eq!(state.library.games.len(), 40);
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_az_row_files_games_and_jumps_to_them() {
        use crate::gamelib::{Game, Known, Library};
        assert_eq!(az_label(0), "0-9");
        assert_eq!(az_label(1), "#");
        assert_eq!(az_label(2), "A");
        assert_eq!(az_label(AZ_BUCKETS - 1), "Z");

        // Filed by the first character of the name shown, whatever case.
        assert_eq!(az_bucket_of("Turrican"), az_bucket_of("turrican"));
        assert_eq!(az_bucket_of("Zool"), AZ_BUCKETS - 1);
        assert_eq!(az_bucket_of("1943"), 0);
        assert_eq!(az_bucket_of("+4 Bonus"), 1, "neither letter nor digit");
        // Folded the way the panel draws it: "Élite" reads as "Elite" and
        // sorts among the E's, so it answers to E.
        assert_eq!(az_bucket_of("Élite"), az_bucket_of("Elite"));
        assert_eq!(az_bucket_of("Ätzen"), az_bucket_of("Atzen"));
        // Something with no ASCII at all still has somewhere to go.
        assert_eq!(az_bucket_of("中"), 1);
        assert_eq!(az_bucket_of(""), 1, "nothing to file it under");

        let named = |name: &str| {
            Some(Game {
                name: name.to_string(),
                ..Game::default()
            })
        };
        let known = |file: &str, name: &str| Known {
            file: file.to_string(),
            game: named(name),
            manual: false,
            slave_sha1: None,
        };
        let mut state = LauncherState::new(MachineSetup::default());
        state.library.db.set_known(vec![
            known("a.lha", "Alien Breed"),
            known("t1.lha", "Turrican"),
            known("t2.lha", "Turrican II"),
            known("z.lha", "Zool"),
        ]);
        state.library.games = Library::known(Path::new("/games"), &state.library.db);

        let present = state.az_buckets_present();
        assert!(present[az_bucket_of("Alien Breed")]);
        assert!(present[az_bucket_of("Turrican")]);
        assert!(!present[0], "no game starts with a digit");
        assert!(!present[az_bucket_of("Xenon")], "nothing under X");

        // Jumping lands on the first of that letter, and chooses it.
        state.jump_to_bucket(az_bucket_of("Turrican"), 2);
        let at = state.library.selected;
        assert_eq!(state.library.games.entries()[at].title(), "Turrican");
        assert_eq!(state.library.scroll, at, "it is put at the top of the box");

        // A letter with nothing under it does nothing at all.
        let before = state.library.selected;
        state.jump_to_bucket(az_bucket_of("Xenon"), 2);
        assert_eq!(state.library.selected, before);
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn a_version_is_offered_only_for_a_named_game_the_library_holds_twice() {
        use crate::gamelib::{Game, Known, Library};
        let named = |name: &str| {
            Some(Game {
                name: name.to_string(),
                year: Some("1994".to_string()),
                ..Game::default()
            })
        };
        let known = |file: &str, game: Option<Game>| Known {
            file: file.to_string(),
            game,
            manual: false,
            slave_sha1: None,
        };
        let mut state = LauncherState::new(MachineSetup::default());
        state.library.db.set_known(vec![
            // The same game packed twice, which is what a version is for.
            known("CannonFodder2_v1.11_0104.lha", named("Cannon Fodder 2")),
            known("CannonFodder2_v1.12_Fr_2578.zip", named("Cannon Fodder 2")),
            // Held once: nothing to tell apart.
            known("Turrican_v1.3_0087.lha", named("Turrican")),
            // Two the scan could not name. They share a title only because
            // the title falls back to the file name.
            known("Mystery/Thing.lha", None),
            known("Elsewhere/Thing.zip", None),
        ]);
        state.library.games = Library::known(Path::new("/games"), &state.library.db);
        state.library.db_loaded = true;

        let version_of = |state: &mut LauncherState, title: &str| {
            let at = state
                .library
                .games
                .entries()
                .iter()
                .position(|e| e.title() == title)
                .expect("the entry is in the library");
            state.select_library_game(at);
            assert!(state.open_meta_editor(), "the editor opens");
            let value = state
                .meta
                .as_ref()
                .unwrap()
                .value(MetaField::Version)
                .to_string();
            state.meta = None;
            value
        };

        // The package's own name, and not how it was packed: `.lha`
        // against `.zip` is the same on both and says nothing about which
        // release either one is.
        assert_eq!(
            version_of(&mut state, "Cannon Fodder 2"),
            "CannonFodder2_v1.11_0104"
        );
        // Held once, so there is nothing to separate it from.
        assert_eq!(version_of(&mut state, "Turrican"), "");
        // Unnamed by the scan: a file name under a row that already says
        // nothing is not the answer to which release it is.
        assert_eq!(version_of(&mut state, "Thing"), "");
    }

    #[test]
    fn a_held_scroll_climbs_five_stages_and_starts_again_when_let_go() {
        use std::time::{Duration, Instant};
        // What a held button does: a step every 60 ms, which is what the
        // stages have to be read against -- they are a second of holding
        // each, not a count of steps.
        const EVERY: Duration = Duration::from_millis(60);
        let start = Instant::now();
        let mut rate = ScrollRate::default();

        // The first step of a run always moves one row, so a click to nudge
        // the list by one does that and nothing more.
        assert_eq!(rate.rows_for_step(start), 1);

        // Held, it works through a stage a second. Sampled just after each
        // second, which is where the stage that second belongs to shows.
        let mut at = start;
        let mut seen = Vec::new();
        for second in 0..5 {
            let until = start + Duration::from_millis(second * 1000 + 500);
            let mut rows = 0;
            while at < until {
                rows = rate.rows_for_step(at);
                at += EVERY;
            }
            seen.push(rows);
        }
        assert_eq!(seen, vec![1, 3, 7, 14, 24]);

        // The last stage is the ceiling: holding it longer does not keep
        // making it faster.
        for _ in 0..100 {
            assert_eq!(rate.rows_for_step(at), 24, "past the last stage");
            at += EVERY;
        }

        // Letting go and pressing again starts from the bottom: a gap
        // longer than a repeat says the run ended...
        at += Duration::from_millis(400);
        assert_eq!(rate.rows_for_step(at), 1, "a pause ended the run");

        // ...and so does a press that says so outright, which is what the
        // mouse does, since a quick re-click can land inside that gap.
        for _ in 0..80 {
            at += EVERY;
            rate.rows_for_step(at);
        }
        assert!(rate.rows_for_step(at) > 1, "mid-run");
        rate.reset();
        assert_eq!(rate.rows_for_step(at), 1);
    }

    #[test]
    fn a_caret_edits_where_it_stands() {
        let mut text = "Golden Axe".to_string();
        let mut caret = Caret::end_of(&text);
        assert_eq!(caret.at(), 10);

        // Typing goes in at the caret and the caret steps over it.
        caret.left();
        caret.left();
        caret.left();
        for c in "the ".chars() {
            caret.insert(&mut text, c);
        }
        assert_eq!(text, "Golden the Axe");
        assert_eq!(caret.at(), 11);

        // Backspace takes what is behind, Delete what is under, and
        // neither moves the other's way.
        assert!(caret.backspace(&mut text));
        assert_eq!(text, "Golden theAxe");
        assert_eq!(caret.at(), 10);
        assert!(caret.delete(&mut text));
        assert_eq!(text, "Golden thexe");
        assert_eq!(caret.at(), 10, "delete leaves the caret where it was");

        // Neither runs off its end.
        caret.home();
        assert!(!caret.backspace(&mut text));
        caret.end(&text);
        assert!(!caret.delete(&mut text));
        assert_eq!(text, "Golden thexe");

        // Stepping stops at both ends rather than wrapping.
        caret.home();
        caret.left();
        assert_eq!(caret.at(), 0);
        caret.end(&text);
        caret.right(&text);
        assert_eq!(caret.at(), text.chars().count());
        let _ = &text;

        // Characters, not bytes: a title with an accent in it steps one
        // letter at a time and is never cut in half.
        let mut text = "Ishido".to_string();
        let mut caret = Caret::end_of(&text);
        caret.left();
        caret.insert(&mut text, 'ó');
        assert_eq!(text, "Ishidóo");
        assert_eq!(caret.at(), 6);
        assert!(caret.backspace(&mut text));
        assert_eq!(text, "Ishido");
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_sign_in_caret_steps_through_the_mask() {
        let mut login = LoginDialog::default();
        for c in "hobbo".chars() {
            login.insert(c);
        }
        assert_eq!(login.user, "hobbo");
        assert_eq!(login.caret.at(), 5);

        // Correcting the middle of a name, without retyping the end.
        login.caret_move(CaretMove::Left);
        login.insert('9');
        login.insert('1');
        assert_eq!(login.user, "hobb91o");
        login.delete();
        assert_eq!(login.user, "hobb91");

        // The password is edited the same way, through the mask: the caret
        // counts characters, and the text itself never comes back out.
        login.focus_on(LoginField::Pass);
        assert_eq!(login.caret.at(), 0, "an empty box starts at the front");
        for c in "sekrit".chars() {
            login.insert(c);
        }
        assert_eq!(login.pass.chars(), 6);
        login.caret_move(CaretMove::Home);
        login.insert('X');
        assert_eq!(login.pass.expose(), "Xsekrit");
        assert_eq!(login.caret.at(), 1);
        login.backspace();
        assert_eq!(login.pass.expose(), "sekrit");
        assert_eq!(login.caret.at(), 0);
        // Backspace at the front does nothing, rather than eating forwards.
        login.backspace();
        assert_eq!(login.pass.expose(), "sekrit");
        login.delete();
        assert_eq!(login.pass.expose(), "ekrit");

        // Moving between boxes puts the caret at the end of the one moved
        // to, not wherever it was in the one left behind.
        login.caret_move(CaretMove::End);
        login.focus_on(LoginField::User);
        assert_eq!(login.caret.at(), login.user.chars().count());
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_metadata_caret_amends_a_field_in_place() {
        let mut meta = MetaDialog {
            file: "Turrican.lha".to_string(),
            ..Default::default()
        };
        *meta.value_mut(MetaField::Name) = "Turican".to_string();
        *meta.value_mut(MetaField::Year) = "1990".to_string();
        meta.focus_on(MetaField::Name);
        assert_eq!(meta.caret.at(), 7);

        // The missing letter goes in where it belongs.
        for _ in 0..4 {
            meta.caret_move(CaretMove::Left);
        }
        meta.insert('r', 64);
        assert_eq!(meta.value(MetaField::Name), "Turrican");

        // A full box takes nothing more, wherever the caret is.
        meta.caret_move(CaretMove::Home);
        meta.insert('X', 8);
        assert_eq!(meta.value(MetaField::Name), "Turrican");

        // Moving on lands the caret at the end of the next box, and never
        // past the end of a shorter one.
        meta.focus_on(MetaField::Year);
        assert_eq!(meta.caret.at(), 4);
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_favourites_list_scrolls_and_stays_inside_itself() {
        let mut state = LauncherState::new(MachineSetup::default());
        for at in 0..10 {
            state
                .library
                .db
                .toggle_favourite(&format!("Game{at}.lha"), &format!("Game {at}"));
        }
        assert_eq!(state.library.db.favourite_count(), 10);

        // Four rows on screen: the scroll stops with the last four in it
        // rather than running off the end into blank rows.
        state.scroll_favourites(3, 4);
        assert_eq!(state.library.favourite_scroll, 3);
        state.scroll_favourites(100, 4);
        assert_eq!(state.library.favourite_scroll, 6);
        state.scroll_favourites(-100, 4);
        assert_eq!(state.library.favourite_scroll, 0);

        // Walking the list with the keyboard drags the window after it.
        state.library.focus = LibraryFocus::Favourites;
        for _ in 0..9 {
            state.step_library_focus(1, 4);
        }
        assert_eq!(state.library.favourite_selected, 9);
        assert_eq!(state.library.favourite_scroll, 6, "the last row is drawn");
        state.step_library_focus(-9, 4);
        assert_eq!(state.library.favourite_selected, 0);
        assert_eq!(state.library.favourite_scroll, 0);

        // Removing from the end takes the selection and the window with it,
        // so neither is left pointing past a list that just got shorter.
        state.library.favourite_selected = 9;
        state.library.favourite_scroll = 6;
        state.remove_favourite(9);
        assert_eq!(state.library.db.favourite_count(), 9);
        assert_eq!(state.library.favourite_selected, 8);
        assert_eq!(state.library.favourite_scroll, 6);
    }

    #[cfg(feature = "game-library")]
    #[test]
    fn the_whdload_entry_sits_between_zorro_and_av() {
        // Where it is, and that turning it off takes out that one entry
        // and leaves every other in place.
        let with: Vec<LauncherTab> = tabs(true).to_vec();
        let without: Vec<LauncherTab> = tabs(false).to_vec();

        let at = with
            .iter()
            .position(|&t| t == LauncherTab::WhdloadLibrary)
            .expect("the strip carries WHDLoad");
        assert_eq!(with[at - 1], LauncherTab::Zorro);
        assert_eq!(with[at + 1], LauncherTab::AvAudio);

        // Off, it is gone -- and nothing else moved relative to itself.
        assert!(!without.contains(&LauncherTab::WhdloadLibrary));
        let mut minus = with.clone();
        minus.remove(at);
        assert_eq!(minus, without, "turning it off moved something else");
    }

    #[test]
    fn whdload_is_its_own_strip_entry_and_opens_on_the_library() {
        // It left Storage: the nav row there no longer offers it, and
        // neither page returns to it.
        assert!(!LauncherTab::Storage
            .nav_options()
            .iter()
            .any(|&(_, tab)| tab == LauncherTab::Whdload));
        // Both pages light the one entry, which is the Library -- what the
        // strip opens on, and what the nav row lists first.
        assert_eq!(
            LauncherTab::Whdload.strip_tab(),
            LauncherTab::WhdloadLibrary
        );
        assert_eq!(
            LauncherTab::WhdloadLibrary.strip_tab(),
            LauncherTab::WhdloadLibrary
        );
        assert_eq!(LauncherTab::Whdload.parent_tab(), None);
        assert_eq!(LauncherTab::WhdloadLibrary.parent_tab(), None);
        assert_eq!(
            LauncherTab::WhdloadLibrary.nav_options().first(),
            Some(&("Library", LauncherTab::WhdloadLibrary))
        );
        // And the entry is in the strip when WHDLoad is on, and gone when
        // it is off.
        assert!(tabs(true).contains(&LauncherTab::WhdloadLibrary));
        assert!(!tabs(false).contains(&LauncherTab::WhdloadLibrary));
        // The row table: the game to launch, then what staging draws on.
        // The last two belong to the library, so they are only here in a
        // build that has one.
        let rows = rows(
            LauncherTab::Whdload,
            ParallelDevice::None,
            SerialMode::Off,
            false,
            false,
        );
        let labels: Vec<&str> = rows.iter().map(|r| r.label).collect();
        // What to boot and how first, then the places things live.
        let mut want = vec!["WHDLoad Settings:", "Launch game", "Machine type"];
        if cfg!(feature = "game-library") {
            want.push("OpenRetro");
        }
        want.extend([
            "Directories:",
            "WHDLoad package",
            "SKick package",
            "Kickstart ROMs",
        ]);
        if cfg!(feature = "game-library") {
            want.push("Game library");
        }
        want.push("Save data");
        assert_eq!(labels, want);
        // Every path row is a Drive row, so the whole host path shows.
        assert!(rows
            .iter()
            .filter(|r| r.field.is_whdload_path_field())
            .all(|r| r.kind == RowKind::Drive));
        // Directories browse as folders; the archives browse as files.
        assert!(!F::WhdloadGame.is_whdload_dir_field());
        assert!(F::WhdloadGame.is_whdload_archive_field());
        assert!(F::WhdloadWhdPackage.is_whdload_archive_field());
        assert!(F::WhdloadSkickPackage.is_whdload_archive_field());
        assert!(F::WhdloadKickstarts.is_whdload_dir_field());
        assert!(F::WhdloadLibrary.is_whdload_dir_field());
        // The game library is a folder of packages, so it browses as one.
        // Picking it with a file chooser is how it stayed unset.
        #[cfg(feature = "game-library")]
        assert!(F::WhdloadGames.is_whdload_dir_field());
    }

    #[test]
    fn an_invalid_drive_name_is_reported_and_keeps_the_field_focused() {
        let mut state = LauncherState::from_raw(&RawConfig {
            filesys: vec![raw_mount("/host0")],
            ..RawConfig::default()
        });
        state.begin_edit_drive_name(LauncherField::Filesys0Dir);
        state.edit_buffer.clear();
        for c in "Work:1".chars() {
            state.edit_push(c);
        }
        state.edit_commit();
        let status = state.status.as_ref().expect("invalid name is reported");
        assert_eq!(status.kind, StatusKind::Error);
        assert!(status.text.contains("invalid character"), "{}", status.text);
        assert_eq!(state.editing(), Some(EditTarget::DriveName(F::Filesys0Dir)));

        // Fixing the name commits it.
        state.edit_backspace();
        state.edit_backspace();
        state.edit_commit();
        assert!(state.editing().is_none());
        assert_eq!(state.setup.drive_name(F::Filesys0Dir), Some("Work"));
    }

    #[test]
    fn default_setup_is_the_a500_aros_machine() {
        let s = MachineSetup::default();
        assert_eq!(s.model, None);
        // With no profile chosen the picker highlights the A500 (the default
        // machine is the A500 defaults).
        assert_eq!(s.selected_model(), MachineModel::A500);
        assert_eq!(s.chipset, Chipset::Ecs);
        assert_eq!(s.cpu, CpuModel::M68000);
        assert_eq!(s.chip_ram, 512 * 1024);
        assert_eq!(s.slow_ram, 512 * 1024);
        assert!(s.rom.is_none(), "boot ROM defaults to bundled AROS");
        // The base A500 had no battery-backed clock.
        assert!(!s.toggle_value(LauncherField::Rtc));
        // The greyed Zorro III RAM explains why on this 24-bit machine.
        assert_eq!(
            s.disabled_reason(LauncherField::Z3Ram),
            Some("needs 32-bit CPU")
        );
        // A bare default emits no overrides at all.
        let toml = s.to_toml().unwrap();
        assert!(toml.trim().is_empty(), "expected empty TOML, got:\n{toml}");
        assert!(s.build_config().is_ok());
    }

    #[test]
    fn launcher_cycles_to_the_68060_with_50mhz_defaults() {
        let mut s = MachineSetup::default();
        for _ in 0..CPUS.len() {
            if s.cpu == CpuModel::M68060 {
                break;
            }
            s.cycle(LauncherField::Cpu, true);
        }
        assert_eq!(s.cpu, CpuModel::M68060, "cycled to the 68060");
        assert_eq!(s.clock_mhz, 50.0, "50 MHz default");
        assert!(s.fpu, "on-die FPU defaults on");
        assert_eq!(s.disabled_reason(LauncherField::Icache), None);
        assert_eq!(s.disabled_reason(LauncherField::Dcache), None);
        assert!(s.toggle_value(LauncherField::Icache));
        assert!(s.toggle_value(LauncherField::Dcache));
    }

    #[test]
    fn launcher_exposes_both_cache_toggles_for_the_68040() {
        let mut s = MachineSetup::default();
        // Step the CPU selector along to the 68040.
        for _ in 0..CPUS.len() {
            if s.cpu == CpuModel::M68040 {
                break;
            }
            s.cycle(LauncherField::Cpu, true);
        }
        assert_eq!(s.cpu, CpuModel::M68040, "cycled to the 68040");
        // The 040 has both caches, so neither toggle is greyed and both default
        // on (like the 030) when the part is selected.
        assert_eq!(s.disabled_reason(LauncherField::Icache), None);
        assert_eq!(s.disabled_reason(LauncherField::Dcache), None);
        assert!(s.toggle_value(LauncherField::Icache));
        assert!(s.toggle_value(LauncherField::Dcache));

        // The 68000 has neither; the 68EC020 has only the instruction cache.
        s.cpu = CpuModel::M68000;
        assert!(s.disabled_reason(LauncherField::Icache).is_some());
        assert!(s.disabled_reason(LauncherField::Dcache).is_some());
        s.cpu = CpuModel::M68EC020;
        assert_eq!(s.disabled_reason(LauncherField::Icache), None);
        assert!(s.disabled_reason(LauncherField::Dcache).is_some());
    }

    #[test]
    fn select_model_applies_profile_defaults_and_emits_only_the_profile() {
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A1200));
        assert_eq!(s.chipset, Chipset::Aga);
        assert_eq!(s.cpu, CpuModel::M68EC020);
        assert_eq!(s.chip_ram, 2 * 1024 * 1024);
        // The base A1200 shipped without a populated RTC; the A500+ has one.
        assert!(!s.toggle_value(LauncherField::Rtc));
        s.select_model(Some(MachineModel::A500Plus));
        assert!(s.toggle_value(LauncherField::Rtc));
        s.select_model(Some(MachineModel::A1200));
        let raw = s.to_raw();
        assert_eq!(raw.machine.profile.as_deref(), Some("A1200"));
        // Everything else matches the profile default, so nothing else is set.
        assert!(raw.memory.chip.is_none());
        assert!(raw.cpu.model.is_none());
        assert!(raw.chipset.revision.is_none());
        assert!(s.build_config().is_ok());
    }

    #[test]
    fn mouse_sensitivity_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // The neutral midpoint shows as "Default" and matches the baseline, so
        // nothing is written.
        assert_eq!(s.value_label(LauncherField::MouseSensitivity), "Default");
        assert_eq!(s.to_raw().input.mouse_sensitivity, None);

        // Cycle down twice (step 1): 50 -> 49 -> 48, and it now persists.
        s.cycle(LauncherField::MouseSensitivity, false);
        s.cycle(LauncherField::MouseSensitivity, false);
        assert_eq!(s.value_label(LauncherField::MouseSensitivity), "48");
        assert_eq!(s.to_raw().input.mouse_sensitivity, Some(48));
    }

    #[test]
    fn screen_tint_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Full colour is the baseline, so nothing is written for it.
        assert_eq!(s.value_label(LauncherField::Tint), "Colour");
        assert_eq!(s.to_raw().display.tint, None);

        s.cycle(LauncherField::Tint, true);
        assert_eq!(s.value_label(LauncherField::Tint), "Black & white");
        assert_eq!(s.to_raw().display.tint, Some("bw".to_string()));

        s.cycle(LauncherField::Tint, true);
        assert_eq!(s.value_label(LauncherField::Tint), "Green");
        assert_eq!(s.to_raw().display.tint, Some("green".to_string()));

        // The written config has to load back into the same setting.
        assert_eq!(s.build_config().expect("valid config").tint, Tint::Green);

        // Cycling backwards from the baseline wraps to the end of the list.
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::Tint, false);
        assert_eq!(s.value_label(LauncherField::Tint), "Sepia");
        assert_eq!(s.to_raw().display.tint, Some("sepia".to_string()));
    }

    /// The module is offered as a source only while it is the destination,
    /// and stops being one the moment the output moves elsewhere.
    #[test]
    #[cfg(all(feature = "midi", feature = "mt32"))]
    fn the_mt32_is_a_midi_source_only_while_it_is_the_destination() {
        let mut s = MachineSetup {
            midi_out: Some(crate::config::MIDI_OUT_MT32.to_string()),
            ..MachineSetup::default()
        };
        assert!(s.midi_out_is_mt32());

        // With no host sources at all, the module is still there to pick.
        s.cycle(LauncherField::MidiIn, true);
        assert_eq!(
            s.value_label(LauncherField::MidiIn),
            crate::midi::MIDI_OUT_MT32_LABEL
        );

        // Moving the output off the module takes the input with it:
        // nothing reaches it, so it has nothing left to answer. The module
        // rides at the end of the output list, so one step wraps to None.
        s.cycle(LauncherField::MidiOut, true);
        assert!(!s.midi_out_is_mt32());
        assert_eq!(s.value_label(LauncherField::MidiIn), "None");

        // And it is no longer among the sources to cycle onto.
        s.cycle(LauncherField::MidiIn, true);
        assert_eq!(s.value_label(LauncherField::MidiIn), "None");
    }

    #[test]
    fn the_mt32_rows_appear_only_when_it_is_the_midi_output() {
        let midi_rows = |out: Option<&str>| {
            let s = MachineSetup {
                midi_out: out.map(str::to_string),
                ..MachineSetup::default()
            };
            rows(
                LauncherTab::IoPorts,
                ParallelDevice::None,
                SerialMode::Midi,
                s.midi_out_is_mt32(),
                false,
            )
            .iter()
            .map(|r| r.field)
            .collect::<Vec<_>>()
        };

        // A host endpoint: the ROM pair and the panel are nothing to do with
        // it, so they are not offered.
        let host = midi_rows(Some("Some USB Interface"));
        assert!(host.contains(&LauncherField::MidiOut));
        assert!(!host.contains(&LauncherField::Mt32ControlRom));
        assert!(!host.contains(&LauncherField::Mt32Panel));

        // The built-in synth: both ROMs and the panel.
        let mt32 = midi_rows(Some(crate::config::MIDI_OUT_MT32));
        assert!(mt32.contains(&LauncherField::Mt32ControlRom));
        assert!(mt32.contains(&LauncherField::Mt32PcmRom));
        assert!(mt32.contains(&LauncherField::Mt32Panel));
    }

    #[test]
    fn the_mt32_rom_pair_and_panel_round_trip_through_raw() {
        let mut s = MachineSetup::default();
        assert_eq!(s.to_raw().serial.mt32_control_rom, None);

        s.set_path(
            LauncherField::Mt32ControlRom,
            std::path::PathBuf::from("MT32_CONTROL.ROM"),
        );
        s.set_path(
            LauncherField::Mt32PcmRom,
            std::path::PathBuf::from("MT32_PCM.ROM"),
        );
        // The panel row cycles its two states now, arrows rather than
        // a checkbox.
        s.cycle(LauncherField::Mt32Panel, true);
        assert!(s.toggle_value(LauncherField::Mt32Panel));
        assert_eq!(s.value_label(LauncherField::Mt32Panel), "Enabled");
        s.cycle(LauncherField::Mt32Panel, false);
        assert_eq!(s.value_label(LauncherField::Mt32Panel), "Disabled");
        s.cycle(LauncherField::Mt32Panel, true);

        let raw = s.to_raw();
        assert_eq!(
            raw.serial.mt32_control_rom.as_deref(),
            Some("MT32_CONTROL.ROM")
        );
        assert_eq!(raw.serial.mt32_pcm_rom.as_deref(), Some("MT32_PCM.ROM"));
        assert_eq!(raw.serial.mt32_panel, Some(true));

        let reloaded = MachineSetup::from_raw(&raw).expect("valid raw");
        assert_eq!(
            reloaded.path(LauncherField::Mt32PcmRom),
            Some(std::path::Path::new("MT32_PCM.ROM"))
        );
        assert!(reloaded.toggle_value(LauncherField::Mt32Panel));
    }

    #[test]
    fn menu_scale_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // 1x is the baseline, so nothing is written for it. The launcher has
        // the width to name the size as well as give the figure.
        assert_eq!(s.value_label(LauncherField::MenuScale), "Normal (1x)");
        assert_eq!(s.to_raw().display.menu_scale, None);

        s.cycle(LauncherField::MenuScale, true);
        assert_eq!(s.value_label(LauncherField::MenuScale), "Large (2x)");
        assert_eq!(s.to_raw().display.menu_scale, Some("2x".to_string()));

        // The written config has to load back into the same setting.
        assert_eq!(
            s.build_config().expect("valid config").menu_scale,
            MenuScale::Large
        );
        let reloaded = MachineSetup::from_raw(&s.to_raw()).expect("valid raw");
        assert_eq!(reloaded.value_label(LauncherField::MenuScale), "Large (2x)");
    }

    #[test]
    fn every_bezel_style_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Off is the baseline, so nothing is written for it.
        assert_eq!(
            s.value_label(LauncherField::Bezel),
            BezelStyle::None.menu_label()
        );
        assert_eq!(s.to_raw().display.bezel, None);

        // Cycling reaches every style, and each is written, reloaded and
        // shown back as itself.
        for _ in 0..BezelStyle::MENU_ORDER.len() {
            s.cycle(LauncherField::Bezel, true);
            let style = s.bezel;
            assert_eq!(
                s.build_config().expect("valid config").bezel,
                style,
                "{} did not survive the config",
                style.label()
            );
            let reloaded = MachineSetup::from_raw(&s.to_raw()).expect("valid raw");
            assert_eq!(
                reloaded.value_label(LauncherField::Bezel),
                style.menu_label()
            );
        }
        // A full turn of the cycle comes back to where it started.
        assert_eq!(s.bezel, BezelStyle::None);
    }

    #[test]
    fn the_sticker_folder_survives_a_launcher_round_trip() {
        let mut raw = RawConfig::default();
        raw.display.bezel_stickers = Some("/data/amiga/stickers".into());
        let s = MachineSetup::from_raw(&raw).expect("valid raw");
        // No launcher row edits the folder, so a straight round-trip must
        // carry it: a machine saved or started from the launcher keeps its
        // stickers.
        assert_eq!(
            s.to_raw().display.bezel_stickers.as_deref(),
            Some("/data/amiga/stickers")
        );
        assert_eq!(
            s.build_config()
                .expect("valid config")
                .bezel_stickers
                .as_deref(),
            Some(Path::new("/data/amiga/stickers"))
        );
        // Unset stays unwritten.
        assert_eq!(
            MachineSetup::default().to_raw().display.bezel_stickers,
            None
        );
    }

    #[test]
    fn deinterlace_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // On is the baseline, so nothing is written for it.
        assert!(s.toggle_value(LauncherField::Deinterlace));
        assert_eq!(s.to_raw().display.deinterlace, None);

        s.cycle(LauncherField::Deinterlace, true);
        assert!(!s.toggle_value(LauncherField::Deinterlace));
        assert_eq!(s.to_raw().display.deinterlace, Some(false));

        // The written config has to load back into the same setting.
        assert!(!s.build_config().expect("valid config").deinterlace);
        let reloaded = MachineSetup::from_raw(&s.to_raw()).expect("valid raw");
        assert!(!reloaded.toggle_value(LauncherField::Deinterlace));
    }

    #[test]
    fn mouse_capture_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Click-to-capture is the baseline, so nothing is written for it.
        assert_eq!(s.value_label(LauncherField::MouseCapture), "On click");
        assert_eq!(s.to_raw().input.mouse_capture, None);

        s.cycle(LauncherField::MouseCapture, true);
        assert_eq!(s.value_label(LauncherField::MouseCapture), "Automatic");
        assert_eq!(s.to_raw().input.mouse_capture, Some("auto".to_string()));

        s.cycle(LauncherField::MouseCapture, true);
        assert_eq!(s.value_label(LauncherField::MouseCapture), "Shortcut only");
        assert_eq!(s.to_raw().input.mouse_capture, Some("manual".to_string()));

        // The written config has to load back into the same setting.
        assert_eq!(
            s.build_config().expect("valid config").mouse_capture,
            MouseCapture::Manual
        );
    }

    #[test]
    fn mouse_capture_greys_out_without_a_mouse() {
        let mut s = MachineSetup::default();
        assert_eq!(s.disabled_reason(LauncherField::MouseCapture), None);
        s.port_devices = [PortDevice::Joystick, PortDevice::Joystick];
        assert_eq!(
            s.disabled_reason(LauncherField::MouseCapture),
            Some("No mouse")
        );
    }

    #[test]
    fn mouse_sensitivity_greys_out_without_a_mouse() {
        let mut s = MachineSetup::default();
        // The default A500 has a mouse in port 1, so it is active.
        assert_eq!(s.disabled_reason(LauncherField::MouseSensitivity), None);

        // Neither port a mouse: greyed.
        s.port_devices = [PortDevice::Joystick, PortDevice::Joystick];
        assert_eq!(
            s.disabled_reason(LauncherField::MouseSensitivity),
            Some("No mouse")
        );

        // A mouse in either port re-enables it.
        s.port_devices = [PortDevice::Joystick, PortDevice::Mouse];
        assert_eq!(s.disabled_reason(LauncherField::MouseSensitivity), None);
    }

    #[test]
    fn rtg_card_round_trips_through_raw() {
        // An A4000 hosts Zorro III, so it comes with the card fitted; that
        // matches its baseline, so nothing is written for it.
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A4000));
        assert_eq!(s.rtg, RtgCard::Z3660);
        assert_eq!(s.value_label(LauncherField::Rtg), "Z3660");
        assert!(s.to_raw().rtg.card.is_none());

        // Turning it off differs from the baseline, so it is written, and
        // the written key is what [rtg] card parses back rather than the
        // display label -- the parse is case-forgiving, the round trip
        // should not lean on that.
        s.cycle(LauncherField::Rtg, true);
        assert_eq!(s.rtg, RtgCard::None);
        assert_eq!(s.value_label(LauncherField::Rtg), "None");
        let raw = s.to_raw();
        assert_eq!(raw.rtg.card.as_deref(), Some("none"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.rtg, RtgCard::None);
        assert!(s.build_config().is_ok());

        // A 68000 machine cannot host Z3660, but its Zorro II bus can host a
        // Picasso II family. The selector therefore remains live and round-trips the
        // parser spelling rather than its friendlier display name.
        s.select_model(Some(MachineModel::A500));
        assert_eq!(s.rtg, RtgCard::None);
        assert_eq!(s.disabled_reason(LauncherField::Rtg), None);
        s.cycle(LauncherField::Rtg, true);
        assert_eq!(s.rtg, RtgCard::Picasso2);
        assert_eq!(s.value_label(LauncherField::Rtg), "Picasso II");
        let raw = s.to_raw();
        assert_eq!(raw.rtg.card.as_deref(), Some("picasso2"));
        assert_eq!(MachineSetup::from_raw(&raw).unwrap().rtg, RtgCard::Picasso2);
        assert!(s.build_config().is_ok());

        s.cycle(LauncherField::Rtg, true);
        assert_eq!(s.rtg, RtgCard::Picasso2Plus);
        assert_eq!(s.value_label(LauncherField::Rtg), "Picasso II+");
        let raw = s.to_raw();
        assert_eq!(raw.rtg.card.as_deref(), Some("picasso2plus"));
        assert_eq!(
            MachineSetup::from_raw(&raw).unwrap().rtg,
            RtgCard::Picasso2Plus
        );
        assert!(s.build_config().is_ok());

        // Graffity [Zorro II] is a Zorro II card too, so it cycles on a
        // 68000 machine right after the Picasso II family; the Zorro III
        // cards do not (the cycle wraps back to None instead).
        s.cycle(LauncherField::Rtg, true);
        assert_eq!(s.rtg, RtgCard::GraffityZ2);
        assert_eq!(s.value_label(LauncherField::Rtg), "Graffity Z2");
        let raw = s.to_raw();
        assert_eq!(raw.rtg.card.as_deref(), Some("graffityz2"));
        assert_eq!(
            MachineSetup::from_raw(&raw).unwrap().rtg,
            RtgCard::GraffityZ2
        );
        assert!(s.build_config().is_ok());
        s.cycle(LauncherField::Rtg, true);
        assert_eq!(s.rtg, RtgCard::None);

        // A loaded 1 MB board preserves its fitted VRAM when saved.
        let mut raw = RawConfig::default();
        raw.rtg.card = Some("picasso2".to_string());
        raw.rtg.vram = Some("1M".to_string());
        let s = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(s.rtg_vram_bytes, 1024 * 1024);
        assert_eq!(s.to_raw().rtg.vram.as_deref(), Some("1M"));
    }

    /// The Video page's Scaling picker writes the `[display] scaling` key
    /// its parser reads back, and stays out of the file while it matches
    /// the default.
    #[test]
    fn display_scaling_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        assert_eq!(s.scaling, DisplayScaling::Smooth);
        assert_eq!(s.value_label(LauncherField::Scaling), "Smooth");
        assert!(s.to_raw().display.scaling.is_none());

        s.cycle(LauncherField::Scaling, true);
        assert_eq!(s.scaling, DisplayScaling::Integer);
        assert_eq!(s.value_label(LauncherField::Scaling), "Integer");
        let raw = s.to_raw();
        assert_eq!(raw.display.scaling.as_deref(), Some("integer"));
        assert_eq!(
            MachineSetup::from_raw(&raw).unwrap().scaling,
            DisplayScaling::Integer
        );
        assert!(s.build_config().is_ok());

        // Two modes, so cycling on returns to the default.
        s.cycle(LauncherField::Scaling, true);
        assert_eq!(s.scaling, DisplayScaling::Smooth);
    }

    #[test]
    fn cycling_chip_ram_walks_the_presets() {
        let mut s = MachineSetup::default();
        assert_eq!(s.chip_ram, 512 * 1024);
        s.cycle(LauncherField::ChipRam, true);
        assert_eq!(s.chip_ram, 1024 * 1024);
        s.cycle(LauncherField::ChipRam, true);
        assert_eq!(s.chip_ram, 2 * 1024 * 1024);
        s.cycle(LauncherField::ChipRam, false);
        assert_eq!(s.chip_ram, 1024 * 1024);
    }

    #[test]
    fn agnus_override_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::Agnus, true); // None -> Some(OCS)
        assert_eq!(s.agnus, Some(AgnusRevision::Ocs));
        let raw = s.to_raw();
        assert_eq!(raw.chipset.agnus.as_deref(), Some("OCS"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.agnus, Some(AgnusRevision::Ocs));
    }

    #[test]
    fn serial_tcp_listen_round_trips_through_raw() {
        // A launcher save must not drop the [serial] listen override, whether
        // or not the tab that edits it was ever opened (regression: it was
        // absent from MachineSetup/to_raw altogether).
        let mut raw = RawConfig::default();
        raw.serial.mode = Some("tcp".into());
        raw.serial.listen = Some("0.0.0.0:2323".into());
        let setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(setup.serial_listen.as_deref(), Some("0.0.0.0:2323"));
        let back = setup.to_raw();
        assert_eq!(back.serial.listen.as_deref(), Some("0.0.0.0:2323"));
    }

    #[cfg(feature = "midi")]
    #[test]
    fn serial_mode_cycle_always_offers_tcp_connect() {
        // tcp-connect used to be skipped unless the loaded config already
        // carried a dial-out address, because nothing here could type one.
        // The Connect box can, so every mode is now reachable from the
        // picker whatever the config arrived holding.
        let mut setup = MachineSetup {
            serial_mode: SerialMode::Tcp,
            ..Default::default()
        };
        setup.cycle(LauncherField::SerialMode, true);
        assert_eq!(setup.serial_mode, SerialMode::TcpConnect);

        // And the picker walks the whole list either way round.
        let mut seen: Vec<SerialMode> = Vec::new();
        for _ in 0..SERIAL_MODES.len() * 2 {
            if !seen.contains(&setup.serial_mode) {
                seen.push(setup.serial_mode);
            }
            setup.cycle(LauncherField::SerialMode, false);
        }
        assert_eq!(seen.len(), SERIAL_MODES.len());
    }

    #[cfg(feature = "midi")]
    #[test]
    fn serial_address_rows_appear_only_in_their_tcp_mode() {
        let has = |mode, field| {
            rows(
                LauncherTab::IoPorts,
                ParallelDevice::None,
                mode,
                false,
                false,
            )
            .iter()
            .any(|r| r.field == field)
        };
        // Dialling out needs somewhere to dial; listening needs somewhere to
        // bind. Neither mode carries the other's address, and the modes with
        // no address at all show neither box.
        assert!(has(SerialMode::TcpConnect, LauncherField::SerialConnect));
        assert!(!has(SerialMode::TcpConnect, LauncherField::SerialListen));
        assert!(has(SerialMode::Tcp, LauncherField::SerialListen));
        assert!(!has(SerialMode::Tcp, LauncherField::SerialConnect));
        for mode in [SerialMode::Off, SerialMode::Stdout, SerialMode::Midi] {
            assert!(!has(mode, LauncherField::SerialConnect));
            assert!(!has(mode, LauncherField::SerialListen));
        }
        // Both boxes are free-text rows, so the panel draws and hit-tests
        // them with the value-box widget.
        for (mode, field) in [
            (SerialMode::TcpConnect, LauncherField::SerialConnect),
            (SerialMode::Tcp, LauncherField::SerialListen),
        ] {
            let r = rows(
                LauncherTab::IoPorts,
                ParallelDevice::None,
                mode,
                false,
                false,
            );
            let found = r.iter().find(|r| r.field == field).unwrap();
            assert_eq!(found.kind, RowKind::Text);
            assert!(LauncherState::is_serial_addr(field));
        }
    }

    #[cfg(feature = "midi")]
    #[test]
    fn typing_a_serial_connect_address_sets_it_and_round_trips() {
        let mut state = LauncherState::new(MachineSetup::default());
        state.setup.serial_mode = SerialMode::TcpConnect;
        state.begin_edit_serial_addr(LauncherField::SerialConnect);
        assert_eq!(state.edit_buffer(), "", "an unset box starts empty");
        for c in "bbs.example.com:1337".chars() {
            state.edit_push(c);
        }
        state.edit_commit();
        assert!(state.editing().is_none());
        assert_eq!(
            state.setup.serial_connect.as_deref(),
            Some("bbs.example.com:1337")
        );
        // What was typed is what a Save writes.
        let raw = state.setup.to_raw();
        assert_eq!(raw.serial.mode.as_deref(), Some("tcp-connect"));
        assert_eq!(raw.serial.connect.as_deref(), Some("bbs.example.com:1337"));

        // Re-opening the box starts from the address it holds, not from a
        // placeholder, so an edit is a correction rather than a retype.
        state.begin_edit_serial_addr(LauncherField::SerialConnect);
        assert_eq!(state.edit_buffer(), "bbs.example.com:1337");
        state.edit_cancel();
    }

    #[cfg(feature = "midi")]
    #[test]
    fn a_serial_address_without_a_port_keeps_the_focus() {
        let mut state = LauncherState::new(MachineSetup::default());
        for bad in ["bbs.example.com", "bbs.example.com:sixty", ":1337"] {
            state.begin_edit_serial_addr(LauncherField::SerialConnect);
            for c in bad.chars() {
                state.edit_push(c);
            }
            state.edit_commit();
            assert_eq!(
                state.editing(),
                Some(EditTarget::SerialAddr(LauncherField::SerialConnect)),
                "{bad} was accepted"
            );
            assert!(state.status.is_some(), "{bad} was refused silently");
            assert_eq!(state.setup.serial_connect, None);
            state.edit_cancel();
        }
        // A bracketed IPv6 literal is a host:port even though it is full of
        // colons: only the one after the closing bracket separates the port.
        state.begin_edit_serial_addr(LauncherField::SerialConnect);
        for c in "[::1]:1337".chars() {
            state.edit_push(c);
        }
        state.edit_commit();
        assert!(state.editing().is_none());
        assert_eq!(state.setup.serial_connect.as_deref(), Some("[::1]:1337"));
    }

    #[cfg(feature = "midi")]
    #[test]
    fn emptying_a_serial_address_box_unsets_it() {
        let mut state = LauncherState::new(MachineSetup {
            serial_mode: SerialMode::Tcp,
            serial_listen: Some("0.0.0.0:2323".into()),
            ..Default::default()
        });
        // The box shows what is bound; emptying it returns to the default,
        // which is what the value column then shows.
        assert_eq!(
            state.setup.value_label(LauncherField::SerialListen),
            "0.0.0.0:2323"
        );
        state.begin_edit_serial_addr(LauncherField::SerialListen);
        for _ in 0..64 {
            state.edit_backspace();
        }
        state.edit_commit();
        assert_eq!(state.setup.serial_listen, None);
        assert_eq!(
            state.setup.value_label(LauncherField::SerialListen),
            crate::config::SERIAL_TCP_DEFAULT_LISTEN
        );
        assert!(state.setup.to_raw().serial.listen.is_none());
    }

    #[cfg(feature = "midi")]
    #[test]
    fn a_serial_address_box_takes_only_printable_characters() {
        let mut state = LauncherState::new(MachineSetup::default());
        state.begin_edit_serial_addr(LauncherField::SerialListen);
        // A space is not part of an address, and neither is a control code.
        for c in "127.0.0.1 :\t12\n34".chars() {
            state.edit_push(c);
        }
        assert_eq!(state.edit_buffer(), "127.0.0.1:1234");
        // And the box has an end: a stuck key cannot grow it without bound.
        for _ in 0..SERIAL_ADDR_MAX * 2 {
            state.edit_push('9');
        }
        assert_eq!(state.edit_buffer().chars().count(), SERIAL_ADDR_MAX);
        state.edit_cancel();
    }

    #[test]
    fn serial_tcp_connect_round_trips_through_raw() {
        // Same contract as the listen override: loading and saving carry the
        // dial-out address unchanged when nothing retypes it.
        let mut raw = RawConfig::default();
        raw.serial.mode = Some("tcp-connect".into());
        raw.serial.connect = Some("bbs.example.com:1337".into());
        let setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(setup.serial_mode, SerialMode::TcpConnect);
        assert_eq!(
            setup.serial_connect.as_deref(),
            Some("bbs.example.com:1337")
        );
        let back = setup.to_raw();
        assert_eq!(back.serial.mode.as_deref(), Some("tcp-connect"));
        assert_eq!(back.serial.connect.as_deref(), Some("bbs.example.com:1337"));
    }

    #[test]
    fn parallel_output_round_trips_through_raw() {
        // The launcher has no printer-path editor, so loading and saving must
        // preserve a hand-written capture path (and its implied Printer device)
        // unchanged.
        let mut raw = RawConfig::default();
        raw.parallel.output = Some("captures/printer.raw".into());
        let setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(setup.parallel_device, ParallelDevice::Printer);
        assert_eq!(
            setup.parallel_output.as_deref(),
            Some(std::path::Path::new("captures/printer.raw"))
        );
        let back = setup.to_raw();
        assert_eq!(back.parallel.device.as_deref(), Some("printer"));
        assert_eq!(
            back.parallel.output.as_deref(),
            Some("captures/printer.raw")
        );
    }

    /// The workshop's pages carry every option the image formats have, and
    /// none of them is a machine setting: nothing here may reach the
    /// configuration a Save would write.
    #[test]
    fn the_disk_image_pages_edit_no_machine_setting() {
        let mut state = LauncherState::new(MachineSetup::default());
        let before = state.setup.to_raw();
        for tab in [
            LauncherTab::CreateFloppy,
            LauncherTab::CreateHard,
            LauncherTab::CreateGeometry,
        ] {
            for r in rows(
                tab,
                ParallelDevice::None,
                SerialMode::default(),
                false,
                false,
            )
            .iter()
            {
                // The page heading is inert and carries no field.
                if r.kind == RowKind::SectionHeader {
                    continue;
                }
                assert!(
                    LauncherState::is_workshop(r.field),
                    "{:?} is not a workshop field",
                    r.field
                );
                // Work every control the row offers, whichever kind it is:
                // a control added here later must be worked here too, or
                // this stops proving anything.
                state.workshop_cycle(r.field, true);
                state.workshop_cycle(r.field, false);
                state.workshop_toggle_flip(r.field);
                for family in FsFamily::ALL {
                    state.workshop_set_fs_family(r.field, family);
                }
                for variant in crate::diskimage::Variant::ALL {
                    state.workshop_set_fs_variant(r.field, variant);
                }
                // Typing into it, and pressing it if it is a button.
                state.begin_edit_new_image(r.field);
                for c in "XY9".chars() {
                    state.edit_push(c);
                }
                state.edit_commit();
                state.edit_cancel();
            }
        }
        // And the two things the workshop can be asked to work out for
        // itself, which reach further than a single row.
        state.workshop.geometry_from_size();
        let _ = state.workshop.hard_spec();
        let _ = state.workshop.floppy_spec();

        assert_eq!(
            state.setup.to_raw(),
            before,
            "the workshop changed the machine configuration"
        );
        // The saved file is that same structure, so nothing here can reach
        // a TOML key by another route either.
        assert_eq!(
            state.setup.to_toml().unwrap(),
            before.to_toml_string().unwrap()
        );
    }

    /// Each picker reaches every value it offers and comes back, and the
    /// value column always has something to say.
    #[test]
    fn the_disk_image_pickers_cover_their_choices() {
        use crate::diskimage::{Container, Density, FileSystem, Partitioning};
        let mut state = LauncherState::new(MachineSetup::default());

        let mut seen = std::collections::HashSet::new();
        for _ in 0..Density::ALL.len() * 2 {
            seen.insert(state.workshop.density);
            state.workshop_cycle(F::NewFloppyDensity, true);
        }
        assert_eq!(seen.len(), Density::ALL.len());

        let mut seen = std::collections::HashSet::new();
        for _ in 0..Container::ALL.len() * 2 {
            seen.insert(state.workshop.container);
            state.workshop_cycle(F::NewFloppyContainer, true);
        }
        assert_eq!(seen.len(), Container::ALL.len());

        // Unformatted, then all eight DOS tags, from the two tick rows.
        let mut seen = std::collections::HashSet::new();
        state.workshop_set_fs_family(F::NewFloppyFs, FsFamily::Unformatted);
        seen.insert(state.workshop.floppy_fs.map(|f| f.dos_type()));
        for family in [FsFamily::Ofs, FsFamily::Ffs] {
            for variant in crate::diskimage::Variant::ALL {
                state.workshop_set_fs_family(F::NewFloppyFs, family);
                set_dostype(&mut state, F::NewFloppyFs, variant);
                seen.insert(state.workshop.floppy_fs.map(|f| f.dos_type()));
            }
        }
        assert_eq!(seen.len(), 9, "unformatted plus DOS0..DOS7");
        assert!(seen.contains(&None));

        let mut seen = std::collections::HashSet::new();
        for _ in 0..Partitioning::ALL.len() * 2 {
            seen.insert(state.workshop.partitioning);
            state.workshop_cycle(F::NewHardPartitioning, true);
        }
        assert_eq!(seen.len(), Partitioning::ALL.len());

        // The size steppers stop at each end of what the box accepts, and
        // the unit swaps without touching the number.
        state.workshop.size = 1;
        state.workshop_cycle(F::NewHardSize, false);
        assert_eq!(state.workshop.size, 1, "size stepped below one");
        state.workshop.size = NEW_HARD_SIZE_MAX;
        state.workshop_cycle(F::NewHardSize, true);
        assert_eq!(
            state.workshop.size, NEW_HARD_SIZE_MAX,
            "size stepped past the max"
        );
        state.workshop.size = 8;
        state.workshop.size_unit = SizeUnit::Mb;
        assert_eq!(state.workshop.bytes(), 8 << 20);
        state.workshop.flip_size_unit();
        assert_eq!(
            state.workshop.size, 8,
            "flipping the unit changed the number"
        );
        assert_eq!(state.workshop.bytes(), 8 << 30);
        state.workshop.flip_size_unit();
        assert_eq!(state.workshop.size_unit, SizeUnit::Mb);

        // Every row shows something in its value column.
        for tab in [LauncherTab::CreateFloppy, LauncherTab::CreateHard] {
            for r in rows(
                tab,
                ParallelDevice::None,
                SerialMode::default(),
                false,
                false,
            )
            .iter()
            {
                match r.kind {
                    RowKind::Cycle | RowKind::Text | RowKind::Size => assert!(
                        !state.row_value(r.field).is_empty(),
                        "{:?} shows nothing",
                        r.field
                    ),
                    RowKind::Action => assert!(
                        !state.workshop_action_label(r.field).is_empty(),
                        "{:?} has no button wording",
                        r.field
                    ),
                    _ => {}
                }
            }
        }
        let _ = FileSystem::OFS;
    }

    /// What the pages describe is what gets built, and the rows that mean
    /// nothing for a given choice grey out.
    #[test]
    fn the_disk_image_pages_describe_what_gets_built() {
        use crate::diskimage::Partitioning;
        let mut state = LauncherState::new(MachineSetup::default());

        // Defaults: a plain OFS floppy, and an RDB hard drive with FFS.
        assert_eq!(
            state.workshop.floppy_spec().filesystem,
            Some(crate::diskimage::FileSystem::OFS)
        );
        let hard = state.workshop.hard_spec();
        assert_eq!(hard.partitioning, Partitioning::Rdb);
        assert_eq!(hard.device, "DH0");
        assert_eq!(hard.bytes, state.workshop.bytes());

        // An unformatted floppy has nothing to boot and nothing to name.
        state.workshop_set_fs_family(F::NewFloppyFs, FsFamily::Unformatted);
        assert!(!state.workshop_applies(F::NewFloppyBootable));
        assert!(!state.workshop_applies(F::NewFloppyLabel));
        // ...and asking for bootable anyway cannot produce boot code.
        state.workshop.floppy_bootable = true;
        assert!(!state.workshop.floppy_spec().bootable);

        // Without a partition table there is no entry to carry a device
        // name or a boot flag.
        state.workshop.partitioning = Partitioning::None;
        assert!(!state.workshop_applies(F::NewHardDevice));
        assert!(!state.workshop_applies(F::NewHardBootable));

        // Geometry follows the size until it is set by hand, and once it
        // is, the size can move without disturbing it.
        assert!(!state.workshop.geometry_custom);
        state.workshop.size = 100;
        let derived = state.workshop.effective_geometry();
        assert_eq!(
            derived,
            crate::diskimage::Geometry::for_size(state.workshop.bytes())
        );
        state.workshop.geometry_from_size();
        state.workshop.geometry_custom = true;
        state.workshop.size = 200;
        assert_eq!(state.workshop.effective_geometry(), derived);

        // The suggested file name follows the volume and the kind.
        state.workshop.floppy_label = "My Disk".into();
        assert_eq!(state.workshop.suggested_name(true), "MyDisk.adf");
        state.workshop.hard_label = String::new();
        assert_eq!(state.workshop.suggested_name(false), "image.hdf");
    }

    /// Drive the workshop pages the way a user does -- type into the boxes,
    /// walk the pickers, flip the ticks -- and check every one of those
    /// choices survives all the way into the bytes on disk. A setting that
    /// looks right on the page and never reaches the image is the one
    /// failure this feature cannot afford.
    #[test]
    fn every_workshop_setting_reaches_the_image() {
        use crate::diskimage::{Container, Density, FileSystem, Partitioning};

        fn scratch(name: &str) -> std::path::PathBuf {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            std::env::temp_dir().join(format!("copperline-workshop-{n}-{name}"))
        }
        fn long(data: &[u8], block: u64, long: usize) -> u32 {
            let at = block as usize * 512 + long * 4;
            u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
        }
        // Clicking a box seeds it with what is already there, so typing a
        // fresh value means clearing it first -- exactly what a user does.
        fn typed(state: &mut LauncherState, field: LauncherField, text: &str) {
            state.begin_edit_new_image(field);
            while !state.edit_buffer().is_empty() {
                state.edit_backspace();
            }
            for c in text.chars() {
                state.edit_push(c);
            }
            state.edit_commit();
            assert_eq!(state.editing(), None, "{field:?} refused \"{text}\"");
        }

        // --- the floppy page ---
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::CreateFloppy;
        state.workshop_cycle(F::NewFloppyDensity, true); // DD -> HD
        state.workshop_cycle(F::NewFloppyContainer, true); // ADF -> extended
        state.workshop_set_fs_family(F::NewFloppyFs, FsFamily::Ffs);
        typed(&mut state, F::NewFloppyLabel, "Scratch");
        state.workshop_toggle_flip(F::NewFloppyBootable);
        let spec = state.workshop.floppy_spec();
        assert_eq!(spec.density, Density::Hd);
        assert_eq!(spec.container, Container::ExtendedAdf);
        assert_eq!(spec.filesystem, Some(FileSystem::FFS));
        assert_eq!(spec.label, "Scratch");
        assert!(spec.bootable);

        let path = scratch("floppy.adf");
        crate::diskimage::create_floppy(&path, &spec).expect("floppy written");
        let adf = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            &adf[0..8],
            b"UAE-1ADF",
            "the extended container was asked for"
        );
        // An extended image is not a plain sector run, so the volume is
        // checked through a plain one written from the same settings.
        let mut plain = spec.clone();
        plain.container = Container::Adf;
        let path = scratch("floppy2.adf");
        crate::diskimage::create_floppy(&path, &plain).expect("floppy written");
        let adf = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(adf.len(), 1_802_240, "HD density");
        assert_eq!(&adf[0..4], b"DOS\x01", "FFS");
        assert!(adf[12..24].iter().any(|&b| b != 0), "boot code was written");
        let root = 1760u64;
        assert_eq!(adf[root as usize * 512 + (128 - 20) * 4], 7, "name length");
        assert_eq!(
            &adf[root as usize * 512 + (128 - 20) * 4 + 1..][..7],
            b"Scratch"
        );

        // --- the hard disk page ---
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::CreateHard;
        typed(&mut state, F::NewHardSize, "1234");
        assert_eq!(state.workshop.size, 1234, "the typed size took");
        state.workshop.flip_size_unit(); // MB -> GB
        state.workshop.flip_size_unit(); // and back, so the run stays quick
        typed(&mut state, F::NewHardSize, "48");
        // OFS with international case folding: DOS2.
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Ofs);
        state.workshop_set_fs_variant(F::NewHardFs, crate::diskimage::Variant::Intl);
        typed(&mut state, F::NewHardDevice, "WORK");
        typed(&mut state, F::NewHardLabel, "Stuff");
        typed(&mut state, F::NewHardBootPri, "-9");
        state.workshop_toggle_flip(F::NewHardReadOnly);

        // Hand-set geometry, reached the way the page reaches it.
        state.workshop.geometry_from_size();
        state.workshop.geometry_custom = true;
        state.tab = LauncherTab::CreateGeometry;
        typed(&mut state, F::NewGeomCylinders, "300");
        typed(&mut state, F::NewGeomSurfaces, "4");
        typed(&mut state, F::NewGeomSectors, "63");
        typed(&mut state, F::NewGeomReserved, "6");

        let spec = state.workshop.hard_spec();
        assert_eq!(spec.partitioning, Partitioning::Rdb);
        assert_eq!(
            spec.filesystem,
            Some(crate::diskimage::FileSystem {
                ffs: false,
                variant: crate::diskimage::Variant::Intl,
            })
        );
        assert_eq!(spec.device, "WORK");
        assert_eq!(spec.label, "Stuff");
        assert_eq!(spec.boot_pri, -9);
        assert_eq!(spec.reserved, 6);
        assert!(spec.read_only);
        assert_eq!(
            spec.geometry,
            Some(crate::diskimage::Geometry {
                cylinders: 300,
                surfaces: 4,
                sectors: 63,
            })
        );

        let path = scratch("hard.hdf");
        let made = crate::diskimage::create_hard(&path, &spec).expect("hard disk written");
        let hdf = std::fs::read(&path).unwrap();
        // Written read-only, so it has to be made writable again to go.
        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert!(perms.readonly(), "the file was marked read only");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut perms = perms;
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let _ = std::fs::remove_file(&path);

        // The geometry decides the size, not the Size box, once it is set
        // by hand: 300 x 4 x 63 blocks of 512.
        assert_eq!(made.bytes, 300 * 4 * 63 * 512);
        assert_eq!(hdf.len() as u64, made.bytes);

        assert_eq!(&hdf[0..4], b"RDSK");
        assert_eq!(long(&hdf, 0, 16), 300, "cylinders");
        assert_eq!(long(&hdf, 0, 17), 63, "sectors");
        assert_eq!(long(&hdf, 0, 18), 4, "surfaces");

        assert_eq!(&hdf[512..516], b"PART");
        assert_eq!(hdf[512 + 36], 4, "device name length");
        assert_eq!(&hdf[512 + 37..512 + 41], b"WORK");
        assert_eq!(long(&hdf, 1, 5), 1, "bootable");
        assert_eq!(long(&hdf, 1, 38), 6, "de_Reserved");
        assert_eq!(long(&hdf, 1, 47) as i32, -9, "de_BootPri");
        assert_eq!(
            long(&hdf, 1, 48),
            crate::diskimage::FileSystem {
                ffs: false,
                variant: crate::diskimage::Variant::Intl,
            }
            .dos_type()
        );

        // And the volume inside the partition carries the name and tag.
        let cyl_blocks = 4 * 63;
        let first = cyl_blocks;
        let blocks = (300 - 1) * cyl_blocks;
        assert_eq!(&hdf[first as usize * 512..][..4], b"DOS\x02");
        let root = first + blocks / 2;
        assert_eq!(hdf[root as usize * 512 + (128 - 20) * 4], 5);
        assert_eq!(
            &hdf[root as usize * 512 + (128 - 20) * 4 + 1..][..5],
            b"Stuff"
        );

        // --- and with the geometry left on Auto, the Size box is what
        // decides, in whichever unit it is showing.
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::CreateHard;
        typed(&mut state, F::NewHardSize, "3");
        state.workshop.flip_size_unit(); // MB -> GB
        assert_eq!(state.workshop.bytes(), 3 * 1024 * 1024 * 1024);
        typed(&mut state, F::NewHardSize, "9");
        state.workshop.flip_size_unit(); // GB -> MB
                                         // No partition table, no filesystem: a brand new mechanism, which
                                         // is also the quickest thing to write.
        while state.workshop.partitioning != Partitioning::None {
            state.workshop_cycle(F::NewHardPartitioning, true);
        }
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Unformatted);
        state.workshop_toggle_flip(F::NewHardSparse);
        let spec = state.workshop.hard_spec();
        assert_eq!(spec.bytes, 9 * 1024 * 1024);
        assert_eq!(spec.geometry, None, "Auto derives it at write time");
        assert!(!spec.sparse, "the tick was cleared, so it is fully written");

        let path = scratch("plain.hdf");
        let made = crate::diskimage::create_hard(&path, &spec).expect("hard disk written");
        let hdf = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // Rounded up to the next whole cylinder, never below what was asked.
        assert!(made.bytes >= 9 * 1024 * 1024);
        assert_eq!(hdf.len() as u64, made.bytes);
        assert!(
            hdf.iter().all(|&b| b == 0),
            "an unpartitioned, unformatted drive carries nothing at all"
        );
    }

    /// Click the DOSType boxes until they describe `want`, from whatever
    /// they were describing. A directory scheme turns the other away while
    /// it is held, so getting from one to the other means clearing first --
    /// which is what the page makes you do too.
    fn set_dostype(
        state: &mut LauncherState,
        field: LauncherField,
        want: crate::diskimage::Variant,
    ) {
        use crate::diskimage::Variant as V;
        for clear in [V::DirCache, V::LongName, V::Intl] {
            if state.workshop_fs_variant_set(field, clear)
                && state.workshop_fs_variant_enabled(field, clear)
            {
                state.workshop_set_fs_variant(field, clear);
            }
        }
        for set in [V::Intl, V::DirCache, V::LongName] {
            let wanted = match set {
                V::Intl => want.is_intl(),
                V::DirCache => want.is_dircache(),
                V::LongName => want.is_longname(),
                V::Plain => false,
            };
            if wanted && !state.workshop_fs_variant_set(field, set) {
                state.workshop_set_fs_variant(field, set);
            }
        }
        if let Some(fs) = state.workshop_fs_of(field) {
            assert_eq!(fs.variant, want, "clicked to the wrong DOS type");
        }
    }

    /// The filesystem picker is two rows of ticks: the family, then the
    /// variants AmigaDOS's own filesystem carries. Between them they have to
    /// reach every DOS tag, and never make one that does not exist.
    #[test]
    fn the_filesystem_ticks_reach_every_dos_tag() {
        use crate::diskimage::Variant;
        let mut state = LauncherState::new(MachineSetup::default());

        // Every tag DOS0..DOS7, from a family tick plus a variant tick.
        let mut seen = std::collections::BTreeSet::new();
        for family in [FsFamily::Ofs, FsFamily::Ffs] {
            for variant in [
                Variant::Plain,
                Variant::Intl,
                Variant::DirCache,
                Variant::LongName,
            ] {
                state.workshop_set_fs_family(F::NewHardFs, family);
                set_dostype(&mut state, F::NewHardFs, variant);
                let fs = state.workshop.hard_fs.expect("a family was chosen");
                assert_eq!(FsFamily::of(Some(fs)), family);
                assert_eq!(fs.variant, variant);
                seen.insert(fs.dos_type());
            }
        }
        assert_eq!(seen.len(), 8, "the two rows reach all eight tags");
        assert_eq!(*seen.first().unwrap(), 0x444F5300);
        assert_eq!(*seen.last().unwrap(), 0x444F5307);

        // Ticking the box that is already set clears it back to plain.
        set_dostype(&mut state, F::NewHardFs, Variant::Plain);
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Ffs);
        state.workshop_set_fs_variant(F::NewHardFs, Variant::Intl);
        assert!(state.workshop_fs_variant_set(F::NewHardFs, Variant::Intl));
        state.workshop_set_fs_variant(F::NewHardFs, Variant::Intl);
        assert!(!state.workshop_fs_variant_set(F::NewHardFs, Variant::Intl));

        // Moving between OFS and FFS keeps the variant: it is one bit of the
        // tag, and dropping the other two with it would surprise.
        set_dostype(&mut state, F::NewHardFs, Variant::DirCache);
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Ofs);
        assert!(state.workshop_fs_variant_set(F::NewHardFs, Variant::DirCache));
        assert_eq!(state.workshop.hard_fs.unwrap().dos_type(), 0x444F5304);

        // Unformatted has no DOS type, so the whole identifiers row goes
        // -- its label with it -- and none of the boxes is lit.
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Unformatted);
        assert_eq!(state.workshop.hard_fs, None);
        assert!(!FsFamily::of(None).has_identifiers());
        assert!(!state.workshop_applies(F::NewHardFsVariant));
        assert!(state.workshop_applies(F::NewHardFs), "the family row stays");
        for variant in [Variant::Intl, Variant::DirCache, Variant::LongName] {
            assert!(!state.workshop_fs_variant_set(F::NewHardFs, variant));
            // ...and clicking one while unformatted does nothing at all.
            state.workshop_set_fs_variant(F::NewHardFs, variant);
            assert_eq!(state.workshop.hard_fs, None);
        }

        // The two pages keep their own choice.
        state.workshop_set_fs_family(F::NewFloppyFs, FsFamily::Ofs);
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Ffs);
        assert!(state.workshop_fs_family_set(F::NewFloppyFs, FsFamily::Ofs));
        assert!(state.workshop_fs_family_set(F::NewHardFs, FsFamily::Ffs));
        assert!(state.workshop_fs_family_set(F::NewHardFsVariant, FsFamily::Ffs));
    }

    /// The DOSType boxes are a picture of the tag, so what they show and
    /// what they accept both come from the tag rather than from a table
    /// written out beside them.
    #[test]
    fn the_dostype_boxes_agree_with_the_tag_they_describe() {
        use crate::diskimage::Variant as V;
        const BOXES: [V; 3] = [V::Intl, V::DirCache, V::LongName];
        let mut state = LauncherState::new(MachineSetup::default());
        state.workshop_set_fs_family(F::NewHardFs, FsFamily::Ffs);

        // Whatever tag is held, each box shows exactly what that tag says
        // about itself, and offers a click only where another tag is
        // reachable by changing that one box.
        for held in V::ALL {
            state.workshop.hard_fs = Some(crate::diskimage::FileSystem {
                ffs: true,
                variant: held,
            });
            for boxed in BOXES {
                let shown = state.workshop_fs_variant_set(F::NewHardFs, boxed);
                let says = match boxed {
                    V::Intl => held.is_intl(),
                    V::DirCache => held.is_dircache(),
                    V::LongName => held.is_longname(),
                    V::Plain => unreachable!("not a box"),
                };
                assert_eq!(shown, says, "{held:?}: the {boxed:?} box");

                // Clicking is offered only when it lands somewhere: a
                // directory scheme carries international with it, so that
                // box cannot be cleared while one is chosen, and the two
                // schemes are one field so neither can join the other.
                let offered = state.workshop_fs_variant_enabled(F::NewHardFs, boxed);
                let reachable = match boxed {
                    V::Intl => !held.is_dircache() && !held.is_longname(),
                    V::DirCache => !held.is_longname(),
                    V::LongName => !held.is_dircache(),
                    V::Plain => unreachable!("not a box"),
                };
                assert_eq!(offered, reachable, "{held:?}: the {boxed:?} box");

                // A click the page will not offer changes nothing.
                if !offered {
                    state.workshop_set_fs_variant(F::NewHardFs, boxed);
                    assert_eq!(state.workshop.hard_fs.unwrap().variant, held);
                }
            }
        }

        // Every tag is reachable by clicking, and no click ever lands on a
        // combination that is not one: three boxes have eight states, the
        // field has four, and the four are the ones the page can produce.
        let mut reached = std::collections::HashSet::new();
        for first in BOXES {
            for second in BOXES {
                for third in BOXES {
                    state.workshop.hard_fs = Some(crate::diskimage::FileSystem {
                        ffs: true,
                        variant: V::Plain,
                    });
                    for click in [first, second, third] {
                        state.workshop_set_fs_variant(F::NewHardFs, click);
                    }
                    let held = state.workshop.hard_fs.unwrap().variant;
                    // Never both schemes at once, however the clicks fell.
                    assert!(!(held.is_dircache() && held.is_longname()));
                    reached.insert(held);
                }
            }
        }
        assert_eq!(
            reached,
            V::ALL.into_iter().collect::<std::collections::HashSet<_>>(),
            "three boxes reach all four tags and nothing else"
        );
    }

    /// The drive identity: what it says by default, what typing into it
    /// does, and that a field never spills into the one beside it.
    #[test]
    fn the_drive_identity_names_itself_until_it_is_told_otherwise() {
        let mut state = LauncherState::new(MachineSetup::default());
        state.tab = LauncherTab::CreateGeometry;

        // Untouched, the drive names itself from its size -- and follows
        // the Size box rather than going stale behind it.
        assert_eq!(state.workshop.identity().vendor, "Amiga");
        assert_eq!(state.workshop.identity().product, "64MB HDF");
        state.workshop.size = 2;
        state.workshop.size_unit = SizeUnit::Gb;
        assert_eq!(state.workshop.identity().product, "2GB HDF");

        // The revision is Copperline's own version, cut to the four bytes
        // the field holds.
        let revision = state.workshop.identity().revision;
        assert!(revision.len() <= crate::harddrive::RDB_IDENTITY_WIDTHS[2]);
        assert!(env!("CARGO_PKG_VERSION").starts_with(&revision));

        // Typing into one field leaves the others deriving, so a Drive
        // typed now does not freeze the Type at today's size.
        state.begin_edit_new_image(F::NewGeomVendor);
        while !state.edit_buffer().is_empty() {
            state.edit_backspace();
        }
        for c in "A600 HD".chars() {
            state.edit_push(c);
        }
        state.edit_commit();
        assert_eq!(state.editing(), None);
        assert_eq!(state.workshop.identity().vendor, "A600 HD");
        assert_eq!(state.workshop.identity().product, "2GB HDF");
        // ...and the Type keeps following the size afterwards, which is the
        // whole reason each field is remembered separately.
        state.workshop.size = 512;
        state.workshop.size_unit = SizeUnit::Mb;
        assert_eq!(state.workshop.identity().vendor, "A600 HD");
        assert_eq!(state.workshop.identity().product, "512MB HDF");

        // Each box stops at the width its RDB field has: a longer string
        // would spill into the next field rather than simply being long.
        for (field, width) in [
            (F::NewGeomVendor, crate::harddrive::RDB_IDENTITY_WIDTHS[0]),
            (F::NewGeomProduct, crate::harddrive::RDB_IDENTITY_WIDTHS[1]),
            (F::NewGeomRevision, crate::harddrive::RDB_IDENTITY_WIDTHS[2]),
        ] {
            state.begin_edit_new_image(field);
            while !state.edit_buffer().is_empty() {
                state.edit_backspace();
            }
            for c in "0123456789ABCDEFGHIJ".chars() {
                state.edit_push(c);
            }
            assert_eq!(state.edit_buffer().chars().count(), width, "{field:?}");
            // And nothing a tool could not print back gets in at all.
            for c in ['\n', '\t', 'é'] {
                let before = state.edit_buffer().to_string();
                state.edit_push(c);
                assert_eq!(state.edit_buffer(), before, "{field:?} took {c:?}");
            }
            state.edit_commit();
        }

        // Auto puts the whole page back to naming itself.
        state.workshop.geometry_from_size();
        assert_eq!(state.workshop.vendor, None);
        assert_eq!(state.workshop.product, None);
        assert_eq!(state.workshop.revision, None);
        assert_eq!(state.workshop.identity().vendor, "Amiga");
        assert_eq!(state.workshop.identity().product, "512MB HDF");
    }

    #[test]
    fn sub_pages_of_hdd_cd() {
        // The sub-pages (CD included) are not top-level strip tabs.
        for t in [
            LauncherTab::Cd,
            LauncherTab::HostFs,
            LauncherTab::HostDisk,
            LauncherTab::BootPriority,
            LauncherTab::Lide,
        ] {
            assert!(!TABS.contains(&t));
            // Each keeps the Storage strip tab highlighted and returns to it.
            assert_eq!(t.strip_tab(), LauncherTab::Storage);
            assert_eq!(t.parent_tab(), Some(LauncherTab::Storage));
        }
        // A top-level tab has no parent.
        assert_eq!(LauncherTab::Storage.parent_tab(), None);

        // The Storage nav lists its sub-pages in order (drawn as a fixed top
        // nav row, not a settings row). Create Image lands on the floppy
        // page, which is the workshop's default half.
        let storage_nav: Vec<_> = LauncherTab::Storage
            .nav_options()
            .iter()
            .map(|&(_, t)| t)
            .collect();
        assert_eq!(
            storage_nav,
            [
                LauncherTab::HostFs,
                LauncherTab::HostDisk,
                LauncherTab::BootPriority,
                LauncherTab::CreateFloppy,
                LauncherTab::Cd,
                LauncherTab::Lide,
            ]
        );

        // The two workshop pages are siblings of each other and children of
        // Storage: their nav row carries a Back button *and* the pair, so a
        // page says both where it came from and which of the two it is.
        for tab in [LauncherTab::CreateFloppy, LauncherTab::CreateHard] {
            assert_eq!(tab.parent_tab(), Some(LauncherTab::Storage));
            assert_eq!(tab.strip_tab(), LauncherTab::Storage);
            assert!(!TABS.contains(&tab), "a workshop page is not a strip tab");
            let nav: Vec<_> = tab.nav_options().iter().map(|&(_, t)| t).collect();
            assert_eq!(
                nav,
                [LauncherTab::CreateFloppy, LauncherTab::CreateHard],
                "{} does not offer both halves",
                tab.label()
            );
        }

        // The Storage tab is just the storage rows (IDE/SCSI options).
        let storage = rows(
            LauncherTab::Storage,
            ParallelDevice::None,
            SerialMode::default(),
            false,
            false,
        );
        assert_eq!(
            storage.first().map(|r| r.field),
            Some(LauncherField::IdeMaster)
        );

        // Each sub-page carries its own rows (its Back button is a fixed top nav
        // element, not a settings row).
        for (tab, marker) in [
            (LauncherTab::HostFs, LauncherField::Filesys0Dir),
            (LauncherTab::Cd, LauncherField::CdImage),
            (LauncherTab::BootPriority, LauncherField::IdeMasterBoot),
            (LauncherTab::Lide, LauncherField::LideBoard),
        ] {
            let page = rows(
                tab,
                ParallelDevice::None,
                SerialMode::default(),
                false,
                false,
            );
            assert!(page.iter().any(|r| r.field == marker));
        }
    }

    #[test]
    fn av_emu_categories() {
        use LauncherField as F;
        // Only "A/V & Emu" (the Audio default) is a strip tab; Video and
        // Emulation are its categories.
        assert!(TABS.contains(&LauncherTab::AvAudio));
        assert!(!TABS.contains(&LauncherTab::AvVideo));
        assert!(!TABS.contains(&LauncherTab::AvEmulation));
        assert!(!TABS.contains(&LauncherTab::AvPaths));
        for t in [
            LauncherTab::AvVideo,
            LauncherTab::AvEmulation,
            LauncherTab::AvPaths,
        ] {
            // They keep the A/V strip entry lit and have no Back button --
            // categories switch between each other via the top nav row.
            assert_eq!(t.strip_tab(), LauncherTab::AvAudio);
            assert_eq!(t.parent_tab(), None);
        }
        // Every A/V page offers the same nav buttons.
        let nav = LauncherTab::AvAudio.nav_options();
        assert_eq!(
            nav,
            [
                ("Audio", LauncherTab::AvAudio),
                ("Video", LauncherTab::AvVideo),
                ("Emulation", LauncherTab::AvEmulation),
                ("Paths", LauncherTab::AvPaths),
            ]
        );
        assert_eq!(LauncherTab::AvVideo.nav_options(), nav);
        assert!(LauncherTab::AvAudio.has_top_nav());
        assert!(LauncherTab::Storage.has_top_nav());
        assert!(!LauncherTab::System.has_top_nav());

        // Each category shows only its own settings; the default is Audio.
        let page = |t| rows(t, ParallelDevice::None, SerialMode::default(), false, false);
        let audio = page(LauncherTab::AvAudio);
        assert!(audio.iter().any(|r| r.field == F::AudioDevice));
        assert!(audio.iter().all(|r| r.field != F::StartFullscreen));
        assert!(page(LauncherTab::AvVideo)
            .iter()
            .any(|r| r.field == F::StartFullscreen));
        assert!(page(LauncherTab::AvEmulation)
            .iter()
            .any(|r| r.field == F::PowerOn));
    }

    #[test]
    fn floppy_rows_hidden_until_wired() {
        use LauncherField as F;
        let with_drives = |n: u8| {
            MachineSetup::from_raw(&toml::from_str(&format!("[floppy]\ndrives = {n}")).unwrap())
                .unwrap()
        };
        let one = with_drives(1);
        assert!(!one.row_hidden(F::Df0Image)); // DF0: is always shown
        assert!(one.row_hidden(F::Df1Image));
        assert!(one.row_hidden(F::Df3WriteProtect));
        let three = with_drives(3);
        assert!(!three.row_hidden(F::Df1Image));
        assert!(!three.row_hidden(F::Df2WriteProtect));
        assert!(three.row_hidden(F::Df3Image));
    }

    #[test]
    fn shader_strength_greys_when_shader_off() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        // The shader is off by default, so its strength does nothing and greys.
        assert_eq!(s.value_label(F::Shader), "Disabled");
        assert_eq!(s.disabled_reason(F::ShaderStrength), Some("Disabled"));
        assert!(!s.applies(F::ShaderStrength));
        // Turning a shader on makes the strength editable again.
        s.cycle(F::Shader, true); // Off -> the first real shader
        assert_ne!(s.value_label(F::Shader), "Disabled");
        assert_eq!(s.disabled_reason(F::ShaderStrength), None);
    }

    #[test]
    fn boot_priority_round_trips_and_greys_empty_slots() {
        use LauncherField as F;
        // A drive carrying a bootpri loads it; an empty slot is greyed.
        let raw: RawConfig = toml::from_str(
            r#"
            [machine]
            model = "A1200"
            [ide]
            master = { path = "wb.hdf", bootpri = 5 }
        "#,
        )
        .unwrap();
        let mut setup = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(setup.value_label(F::IdeMasterBoot), "5");
        assert!(!setup.drive_boot_off(F::IdeMasterBoot));
        assert_eq!(setup.disabled_reason(F::IdeMasterBoot), None);
        // With a drive present the page has editable rows (its info text shows);
        // a machine with no hard-disk drives has none.
        assert!(setup.has_boot_priority_rows());
        assert!(!MachineSetup::default().has_boot_priority_rows());
        // No slave image, so its boot row is greyed and inert.
        assert_eq!(setup.disabled_reason(F::IdeSlaveBoot), Some("No drive"));

        // The arrows step the live priority and re-emit it.
        setup.cycle(F::IdeMasterBoot, true); // 5 -> 6
        assert_eq!(setup.value_label(F::IdeMasterBoot), "6");
        assert_eq!(setup.to_raw().ide.master.as_ref().unwrap().bootpri, Some(6));

        // Unset (None) shows as 0 and stays keyless on save.
        setup.set_drive_bootpri(F::IdeMasterBoot, None);
        assert_eq!(setup.value_label(F::IdeMasterBoot), "0");
        assert_eq!(setup.to_raw().ide.master.as_ref().unwrap().bootpri, None);

        // Clearing the Bootable box writes the -128 sentinel and shows it, so
        // the greyed row says what the config will hold; the priority is kept
        // underneath, and re-ticking restores it.
        setup.set_drive_bootpri(F::IdeMasterBoot, Some(6));
        setup.toggle_drive_boot(F::IdeMasterBoot);
        assert!(setup.drive_boot_off(F::IdeMasterBoot));
        assert_eq!(setup.value_label(F::IdeMasterBoot), "-128");
        assert_eq!(
            setup.to_raw().ide.master.as_ref().unwrap().bootpri,
            Some(-128)
        );
        setup.toggle_drive_boot(F::IdeMasterBoot);
        assert_eq!(setup.value_label(F::IdeMasterBoot), "6");
        assert_eq!(setup.to_raw().ide.master.as_ref().unwrap().bootpri, Some(6));
    }

    #[test]
    fn boot_priority_loads_the_never_sentinel_as_a_cleared_box() {
        use LauncherField as F;
        let raw: RawConfig = toml::from_str(
            r#"
            [machine]
            model = "A1200"
            [ide]
            master = { path = "wb.hdf", bootpri = -128 }
        "#,
        )
        .unwrap();
        let setup = MachineSetup::from_raw(&raw).unwrap();
        assert!(setup.drive_boot_off(F::IdeMasterBoot));
        assert_eq!(
            setup.to_raw().ide.master.as_ref().unwrap().bootpri,
            Some(-128)
        );
    }

    #[test]
    fn boot_priority_cascade_seeds_added_drives() {
        use LauncherField as F;
        let mut setup = MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        // First drive added takes 0 (keyless); the second drops below the
        // floppies to -35, then -40, each written explicitly.
        setup.set_path(F::IdeMaster, PathBuf::from("a.hdf"));
        setup.set_path(F::IdeSlave, PathBuf::from("b.hdf"));
        assert_eq!(setup.value_label(F::IdeMasterBoot), "0");
        assert_eq!(setup.value_label(F::IdeSlaveBoot), "-35");
        assert_eq!(setup.to_raw().ide.master.as_ref().unwrap().bootpri, None);
        assert_eq!(
            setup.to_raw().ide.slave.as_ref().unwrap().bootpri,
            Some(-35)
        );
    }

    #[test]
    fn boot_priority_steps_clamp_and_parse() {
        // The arrows stay within -127..=127 (the -128 sentinel is the box).
        assert_eq!(step_drive_bootpri(Some(127), true), Some(127));
        assert_eq!(step_drive_bootpri(Some(-127), false), Some(-127));
        // Unset steps off from 0.
        assert_eq!(step_drive_bootpri(None, true), Some(1));
        assert_eq!(step_drive_bootpri(None, false), Some(-1));
        // The Priority column shows the number, 0 when unset.
        assert_eq!(drive_bootpri_label(None), "0");
        assert_eq!(drive_bootpri_label(Some(7)), "7");
        // Cascade: rank 0 keyless, then -35, -40, -45.
        assert_eq!(hdd_boot_cascade(0), None);
        assert_eq!(hdd_boot_cascade(1), Some(-35));
        assert_eq!(hdd_boot_cascade(3), Some(-45));
        // Typed entry: blank clears to unset, range enforced, -128 accepted
        // (the commit turns it into a cleared box).
        assert_eq!(parse_drive_bootpri("  "), Ok(None));
        assert_eq!(parse_drive_bootpri("-128"), Ok(Some(-128)));
        assert!(parse_drive_bootpri("200").is_err());
    }

    #[test]
    fn lide_board_cycles_and_round_trips() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        assert_eq!(s.value_label(F::LideBoard), "None");
        assert!(s.to_raw().lide.board.is_none());

        s.cycle(F::LideBoard, true);
        assert_eq!(s.value_label(F::LideBoard), "RIPPLE");
        s.cycle(F::LideBoard, true);
        assert_eq!(s.value_label(F::LideBoard), "RIDE");
        s.cycle(F::LideBoard, true);
        assert_eq!(s.value_label(F::LideBoard), "AT-Bus 2008");

        // A bare board with nothing attached is indistinguishable from no
        // board at all (`LideConfig::enabled`, matching `[scsi]`), so give it
        // a ROM before checking the round trip.
        s.set_path(F::LideRom, PathBuf::from("lide-atbus.rom"));
        let raw = s.to_raw();
        assert_eq!(raw.lide.board.as_deref(), Some("atbus2008"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.value_label(F::LideBoard), "AT-Bus 2008");

        s.cycle(F::LideBoard, true);
        assert_eq!(s.value_label(F::LideBoard), "None");
        assert!(s.to_raw().lide.board.is_none());
    }

    #[test]
    fn lide_rows_are_hidden_without_a_board_and_rom_bank2_hidden_on_atbus2008() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        // Nothing to configure without a board fitted.
        assert!(s.row_hidden(F::LideRom));
        assert!(s.row_hidden(F::LideRomBank2));
        assert!(s.row_hidden(F::LideDrive0));
        assert!(s.row_hidden(F::LideDrive1));

        s.cycle(F::LideBoard, true); // RIPPLE: two channels, four drives
        assert!(!s.row_hidden(F::LideRom));
        assert!(!s.row_hidden(F::LideRomBank2));
        assert!(!s.row_hidden(F::LideDrive0));
        // Drive 1 is gated on drive 0 holding an image: `[lide] drives` is a
        // positional list, so a hole cannot be represented in the config.
        assert!(s.row_hidden(F::LideDrive1));
        s.set_path(F::LideDrive0, PathBuf::from("a.hdf"));
        assert!(!s.row_hidden(F::LideDrive1));
        assert!(s.row_hidden(F::LideDrive2), "drive 1 is still empty");
        s.set_path(F::LideDrive1, PathBuf::from("b.hdf"));
        s.set_path(F::LideDrive2, PathBuf::from("c.hdf"));
        assert!(!s.row_hidden(F::LideDrive3));

        s.cycle(F::LideBoard, true); // RIDE: one channel, two drives, four banks
        assert!(!s.row_hidden(F::LideRomBank2), "RIDE has flash banking too");
        assert!(s.row_hidden(F::LideDrive2), "RIDE has only one channel");
        assert!(s.row_hidden(F::LideDrive3));
        // Cycling the board dropped the drives beyond RIDE's channel count.
        assert!(s.to_raw().lide.drives.len() <= 2);

        s.cycle(F::LideBoard, true); // AT-Bus 2008: one channel, no banking
        assert!(s.row_hidden(F::LideRomBank2));
        assert!(s.row_hidden(F::LideDrive2));
    }

    #[test]
    fn lide_drives_round_trip_in_channel_order_with_boot_priority() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        s.cycle(F::LideBoard, true); // RIPPLE
        s.set_path(F::LideDrive0, PathBuf::from("ch0-master.hdf"));
        s.set_path(F::LideDrive1, PathBuf::from("ch0-slave.hdf"));
        s.set_drive_bootpri(F::LideDrive0Boot, Some(5));

        // Boot priority sits on the Lide page itself, not the shared Boot
        // Priority page (see LIDE_ROWS's comment) -- so `has_boot_priority_rows`
        // (which only tracks IDE/SCSI) is untouched by these two lide drives.
        assert!(!s.has_boot_priority_rows());
        assert_eq!(s.value_label(F::LideDrive0Boot), "5");
        assert_eq!(s.disabled_reason(F::LideDrive1Boot), None);
        assert!(
            s.row_hidden(F::LideDrive2Boot),
            "empty slot stays off the page"
        );

        let raw = s.to_raw();
        assert_eq!(raw.lide.board.as_deref(), Some("ripple"));
        assert_eq!(raw.lide.drives.len(), 2);
        assert_eq!(raw.lide.drives[0].path, "ch0-master.hdf");
        assert_eq!(raw.lide.drives[0].bootpri, Some(5));
        assert_eq!(raw.lide.drives[1].path, "ch0-slave.hdf");

        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.path(F::LideDrive0), Some(Path::new("ch0-master.hdf")));
        assert_eq!(back.path(F::LideDrive1), Some(Path::new("ch0-slave.hdf")));
        assert_eq!(back.value_label(F::LideDrive0Boot), "5");
    }

    #[test]
    fn ide_and_lide_cd_images_label_and_grey_boot_priority_like_scsi() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        s.cycle(F::LideBoard, true); // RIPPLE
        s.set_path(F::IdeMaster, PathBuf::from("game.iso"));
        s.set_path(F::LideDrive0, PathBuf::from("game.cue"));

        assert_eq!(s.value_label(F::IdeMaster), "game.iso (CD-ROM)");
        assert_eq!(s.value_label(F::LideDrive0), "game.cue (CD-ROM)");
        assert_eq!(s.disabled_reason(F::IdeMasterBoot), Some("CD-ROM"));
        assert_eq!(s.disabled_reason(F::LideDrive0Boot), Some("CD-ROM"));

        // A hard disk at the same fields gets no such treatment.
        s.set_path(F::IdeSlave, PathBuf::from("work.hdf"));
        assert_eq!(s.value_label(F::IdeSlave), "work.hdf");
        assert_eq!(s.disabled_reason(F::IdeSlaveBoot), None);
    }

    /// The FFS/OFS toggle applies only to a directory mount on a
    /// disk-backed drive field (IDE/SCSI/lide), never to a `Filesys*Dir`
    /// row -- a live HOSTFS mount is a directory too, but has no filesystem
    /// of its own to choose. `drive_is_directory` is also the cached flag
    /// `launcher_drive_fs_applies` (`ui.rs`) reads instead of statting the
    /// path on every frame; this exercises both that it is scoped correctly
    /// and that it tracks the real path shape as it changes.
    #[test]
    fn drive_filesystem_toggle_applies_only_to_disk_backed_fields_and_tracks_path_shape() {
        use LauncherField as F;
        let dir = std::env::temp_dir().join(format!(
            "copperline-launcher-isdir-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = MachineSetup::default();
        // A live HOSTFS mount points at a real directory too, but the
        // toggle must never claim to apply there.
        s.set_path(F::Filesys0Dir, dir.clone());
        assert!(!s.drive_is_directory(F::Filesys0Dir));

        // A disk-backed field pointed at that same directory: applies.
        s.set_path(F::IdeMaster, dir.clone());
        assert!(s.drive_is_directory(F::IdeMaster));

        // Repointed at an ordinary file: does not apply.
        let file = dir.join("plain.hdf");
        std::fs::write(&file, b"").unwrap();
        s.set_path(F::IdeMaster, file);
        assert!(!s.drive_is_directory(F::IdeMaster));

        // Clearing drops the flag along with the path.
        s.set_path(F::IdeMaster, dir.clone());
        assert!(s.drive_is_directory(F::IdeMaster));
        s.clear_path(F::IdeMaster);
        assert!(!s.drive_is_directory(F::IdeMaster));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lide_clearing_a_drive_cascades_to_later_slots() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        s.cycle(F::LideBoard, true); // RIPPLE
        s.set_path(F::LideDrive0, PathBuf::from("a.hdf"));
        s.set_path(F::LideDrive1, PathBuf::from("b.hdf"));
        s.set_path(F::LideDrive2, PathBuf::from("c.hdf"));
        assert_eq!(s.to_raw().lide.drives.len(), 3);

        // Clearing an earlier slot cannot leave a later one dangling: the
        // config format has no way to represent the resulting gap.
        s.clear_path(F::LideDrive0);
        assert_eq!(s.path(F::LideDrive1), None);
        assert_eq!(s.path(F::LideDrive2), None);
        assert!(s.to_raw().lide.drives.is_empty());
    }

    /// Lide host-disk attachment points are only fitted for a channel the
    /// selected board personality actually has.
    #[test]
    fn lide_host_disk_attach_points_are_fitted_by_board_channel_count() {
        use crate::config::HostDiskAttach;
        use LauncherField as F;
        let mut s = MachineSetup::default();

        // No board at all: nothing is fitted.
        assert!(!s.attach_is_fitted(HostDiskAttach::LideMaster(0)));

        // RIDE: one channel.
        s.cycle(F::LideBoard, true); // RIPPLE
        s.cycle(F::LideBoard, true); // RIDE
        assert!(s.attach_is_fitted(HostDiskAttach::LideMaster(0)));
        assert!(!s.attach_is_fitted(HostDiskAttach::LideMaster(1)));

        // RIPPLE: two channels.
        s.cycle(F::LideBoard, true); // AT-Bus 2008
        s.cycle(F::LideBoard, true); // None
        s.cycle(F::LideBoard, true); // RIPPLE
        assert!(s.attach_is_fitted(HostDiskAttach::LideMaster(0)));
        assert!(s.attach_is_fitted(HostDiskAttach::LideSlave(1)));

        assert_eq!(
            MachineSetup::host_disk_attach_of(F::LideDrive0),
            Some(HostDiskAttach::LideMaster(0))
        );
        assert_eq!(
            MachineSetup::host_disk_attach_of(F::LideDrive1),
            Some(HostDiskAttach::LideSlave(0))
        );
        assert_eq!(
            MachineSetup::host_disk_attach_of(F::LideDrive2),
            Some(HostDiskAttach::LideMaster(1))
        );
        assert_eq!(
            MachineSetup::host_disk_attach_of(F::LideDrive3),
            Some(HostDiskAttach::LideSlave(1))
        );
    }

    /// Mounting a host disk on an earlier lide slot must cascade-clear later
    /// slots exactly like `clear_path` does for the image case -- otherwise
    /// `[lide] drives` (a positional array) would go on carrying images the
    /// UI, and a saved config, no longer have any way to show.
    #[test]
    fn lide_host_disk_on_an_earlier_slot_cascades_to_later_slots() {
        use LauncherField as F;
        let mut s = MachineSetup::default();
        s.cycle(F::LideBoard, true); // RIPPLE
        s.set_path(F::LideDrive0, PathBuf::from("ch0-master.hdf"));
        s.set_path(F::LideDrive1, PathBuf::from("ch0-slave.hdf"));
        s.set_path(F::LideDrive2, PathBuf::from("ch1-master.hdf"));
        assert_eq!(s.to_raw().lide.drives.len(), 3);

        s.set_host_disks_for_test(vec![HostDiskRow {
            id: "disk4".to_string(),
            fingerprint: None,
            volume: "SanDisk".to_string(),
            size: "31.9 GB".to_string(),
            mounted: Vec::new(),
            writable: true,
            attach: Some(crate::config::HostDiskAttach::LideMaster(0)),
        }]);
        s.select_host_disk(0);
        let mounted = s.mount_host_disks().expect("channel 0 master is fitted");
        assert_eq!(mounted.len(), 1);

        // The host disk took slot 0; slots 1 and 2's images must not be left
        // dangling as invisible ghosts behind it.
        assert_eq!(s.path(F::LideDrive1), None);
        assert_eq!(s.path(F::LideDrive2), None);
        assert!(s.to_raw().lide.drives.is_empty());
    }

    /// Cycling the lide board to a personality with fewer channels must drop
    /// host disks on channels the new personality no longer has, exactly as
    /// it already drops image drives there.
    #[test]
    fn lide_board_switch_drops_host_disks_on_lost_channels() {
        use crate::config::HostDiskAttach;
        use LauncherField as F;
        let mut s = MachineSetup {
            lide_board: Some(LidePersonality::Ripple), // two channels
            host_disks_attached: vec![crate::config::HostDiskConfig {
                device: "disk4".to_string(),
                fingerprint: None,
                identity_confirmed: true,
                attach: HostDiskAttach::LideSlave(1), // RIPPLE-only channel
                writable: true,
            }],
            host_disk_selected: vec!["disk4".to_string()],
            ..Default::default()
        };
        assert!(s.host_disk_is_attached("disk4"));

        s.cycle(F::LideBoard, true); // RIDE: one channel only
        assert!(
            !s.host_disk_is_attached("disk4"),
            "channel 1 no longer exists on RIDE"
        );
        assert!(s.host_disks_attached().is_empty());
    }

    #[cfg(feature = "midi")]
    #[test]
    fn io_ports_pages_carry_one_section_each() {
        let header = |tab| {
            let r = rows(tab, ParallelDevice::None, SerialMode::Midi, false, false);
            r.iter()
                .filter(|x| x.kind == RowKind::SectionHeader)
                .map(|x| x.label)
                .collect::<Vec<_>>()
        };
        // Serial Port page: the Device / Mode selector and (in MIDI) the
        // endpoints, under the one heading.
        assert_eq!(
            header(LauncherTab::IoPorts),
            ["Serial:"],
            "the strip tab is the serial page"
        );
        let r = rows(
            LauncherTab::IoPorts,
            ParallelDevice::None,
            SerialMode::Midi,
            false,
            false,
        );
        assert!(r.iter().any(|x| x.field == LauncherField::SerialMode));
        assert!(r.iter().any(|x| x.field == LauncherField::MidiOut));
        assert!(
            !r.iter().any(|x| x.field == LauncherField::ParallelDevice),
            "parallel lives on its own page now"
        );
        // Parallel Port page: the device selector.
        assert_eq!(header(LauncherTab::IoParallel), ["Parallel:"]);
        let r = rows(
            LauncherTab::IoParallel,
            ParallelDevice::None,
            SerialMode::Midi,
            false,
            false,
        );
        assert!(r.iter().any(|x| x.field == LauncherField::ParallelDevice));
        // Networking page: the A2065 board selector.
        assert_eq!(header(LauncherTab::IoNetworking), ["Ethernet:"]);
        let r = rows(
            LauncherTab::IoNetworking,
            ParallelDevice::None,
            SerialMode::Midi,
            false,
            false,
        );
        assert!(r.iter().any(|x| x.field == LauncherField::Ethernet));
        // Audio page: the Toccata board toggle, and (in an `mhi` build)
        // the MHI board toggle alongside it.
        assert_eq!(header(LauncherTab::IoAudio), ["Sound Card:"]);
        let r = rows(
            LauncherTab::IoAudio,
            ParallelDevice::None,
            SerialMode::Midi,
            false,
            false,
        );
        assert!(r.iter().any(|x| x.field == LauncherField::Toccata));
        #[cfg(feature = "mhi")]
        assert!(r.iter().any(|x| x.field == LauncherField::Mhi));
        // The four pages switch between each other on the nav row, under
        // the one strip entry, with no Back button -- the A/V pattern.
        for tab in [
            LauncherTab::IoParallel,
            LauncherTab::IoNetworking,
            LauncherTab::IoAudio,
        ] {
            assert_eq!(tab.strip_tab(), LauncherTab::IoPorts);
            assert_eq!(tab.parent_tab(), None);
            assert!(tab.has_top_nav());
        }
        assert_eq!(
            LauncherTab::IoPorts.nav_options(),
            [
                ("Serial Port", LauncherTab::IoPorts),
                ("Parallel Port", LauncherTab::IoParallel),
                ("Networking", LauncherTab::IoNetworking),
                ("Audio", LauncherTab::IoAudio),
            ]
        );
    }

    #[test]
    fn a2065_board_cycles_and_round_trips() {
        let mut s = MachineSetup::default();
        assert_eq!(s.value_label(LauncherField::Ethernet), "None");
        assert!(s.to_raw().a2065.net.is_none());
        assert!(!s.ethernet_breaks_determinism());

        s.cycle(LauncherField::Ethernet, true);
        assert_eq!(s.value_label(LauncherField::Ethernet), "Isolated");
        s.cycle(LauncherField::Ethernet, true);
        assert_eq!(s.value_label(LauncherField::Ethernet), "Loopback");
        // Loopback echoes frames on the emulated clock: no determinism warning.
        assert!(!s.ethernet_breaks_determinism());

        // The fitted board and its backend survive a save/load round trip.
        let raw = s.to_raw();
        assert_eq!(raw.a2065.net.as_deref(), Some("loopback"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.a2065_net, Some(NetConfig::Loopback));

        if crate::net::NAT_AVAILABLE {
            s.cycle(LauncherField::Ethernet, true);
            assert_eq!(s.value_label(LauncherField::Ethernet), "NAT");
            // NAT carries traffic on the host's schedule.
            assert!(s.ethernet_breaks_determinism());
            assert_eq!(s.to_raw().a2065.net.as_deref(), Some("nat"));
            s.cycle(LauncherField::Ethernet, true);
        } else {
            // Where the NAT cannot come up the picker skips straight past it.
            s.cycle(LauncherField::Ethernet, true);
        }
        assert_eq!(s.value_label(LauncherField::Ethernet), "None");
    }

    #[test]
    fn hostsocket_board_cycles_and_round_trips() {
        let mut s = MachineSetup::default();
        assert_eq!(s.value_label(LauncherField::HostSocket), "None");
        assert!(s.to_raw().hostsocket.net.is_none());
        assert!(!s.ethernet_breaks_determinism());

        s.cycle(LauncherField::HostSocket, true);
        assert_eq!(s.value_label(LauncherField::HostSocket), "Isolated");
        s.cycle(LauncherField::HostSocket, true);
        assert_eq!(s.value_label(LauncherField::HostSocket), "Loopback");
        // HostSocket over loopback is fully deterministic: no warning.
        assert!(!s.ethernet_breaks_determinism());

        // The fitted board and its backend survive a save/load round trip,
        // resolving to the bundled wasm board on the way through Config.
        let raw = s.to_raw();
        assert_eq!(raw.hostsocket.net.as_deref(), Some("loopback"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.hostsocket_net, Some(NetConfig::Loopback));

        if crate::net::NAT_AVAILABLE {
            s.cycle(LauncherField::HostSocket, true);
            assert_eq!(s.value_label(LauncherField::HostSocket), "NAT");
            assert!(s.ethernet_breaks_determinism());
            assert_eq!(s.to_raw().hostsocket.net.as_deref(), Some("nat"));
        }
        s.cycle(LauncherField::HostSocket, true);

        // Host: real host OS sockets, not a NetConfig backend at all -- the
        // one choice this board has that A2065's own picker does not (see
        // cycle_hostsocket_board's own comment). Genuinely non-deterministic
        // (real host sockets, same as NAT/bridge), and address/gateway must
        // not survive alongside it -- Config::from_raw rejects both under
        // net = "host".
        assert_eq!(s.value_label(LauncherField::HostSocket), "Host");
        assert!(s.ethernet_breaks_determinism());
        s.hostsocket_address = Some("192.168.1.50/24".to_string());
        s.hostsocket_gateway = Some("192.168.1.1".to_string());
        let raw = s.to_raw();
        assert_eq!(raw.hostsocket.net.as_deref(), Some("host"));
        assert!(raw.hostsocket.interface.is_none());
        assert!(raw.hostsocket.address.is_none());
        assert!(raw.hostsocket.gateway.is_none());
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert!(back.hostsocket_host_mode);
        assert_eq!(back.value_label(LauncherField::HostSocket), "Host");

        s.cycle(LauncherField::HostSocket, true);
        assert_eq!(s.value_label(LauncherField::HostSocket), "None");
        assert!(!s.hostsocket_host_mode);
    }

    #[test]
    fn toccata_and_mhi_boards_toggle_and_round_trip() {
        let mut s = MachineSetup::default();
        // Off is the baseline for both, so nothing is written until fitted.
        assert!(!s.toggle_value(LauncherField::Toccata));
        #[cfg(feature = "mhi")]
        assert!(!s.toggle_value(LauncherField::Mhi));
        assert_eq!(s.to_raw().toccata.enabled, None);
        #[cfg(feature = "mhi")]
        assert_eq!(s.to_raw().mhi.enabled, None);

        s.cycle(LauncherField::Toccata, true);
        #[cfg(feature = "mhi")]
        s.cycle(LauncherField::Mhi, true);
        assert!(s.toggle_value(LauncherField::Toccata));
        #[cfg(feature = "mhi")]
        assert!(s.toggle_value(LauncherField::Mhi));
        assert_eq!(s.to_raw().toccata.enabled, Some(true));
        #[cfg(feature = "mhi")]
        assert_eq!(s.to_raw().mhi.enabled, Some(true));

        // The written config has to load back into the same settings.
        let cfg = s.build_config().expect("valid config");
        assert!(cfg.toccata);
        #[cfg(feature = "mhi")]
        assert!(cfg.mhi);
        let reloaded = MachineSetup::from_raw(&s.to_raw()).expect("valid raw");
        assert!(reloaded.toggle_value(LauncherField::Toccata));
        #[cfg(feature = "mhi")]
        assert!(reloaded.toggle_value(LauncherField::Mhi));

        s.cycle(LauncherField::Toccata, false);
        #[cfg(feature = "mhi")]
        s.cycle(LauncherField::Mhi, false);
        assert!(!s.toggle_value(LauncherField::Toccata));
        #[cfg(feature = "mhi")]
        assert!(!s.toggle_value(LauncherField::Mhi));
        assert_eq!(s.to_raw().toccata.enabled, None);
        #[cfg(feature = "mhi")]
        assert_eq!(s.to_raw().mhi.enabled, None);
    }

    #[test]
    fn hostsocket_keeps_uneditable_keys_across_a_launcher_save() {
        // dns_server/hostname/address/gateway/resolver have no launcher
        // row; a load-edit-save cycle must carry them through untouched.
        let mut raw = RawConfig::default();
        raw.hostsocket.net = Some("nat".to_string());
        raw.hostsocket.dns_server = Some("192.0.2.53".to_string());
        raw.hostsocket.hostname = Some("boing".to_string());
        raw.hostsocket.address = Some("192.168.1.50/24".to_string());
        raw.hostsocket.gateway = Some("192.168.1.1".to_string());
        raw.hostsocket.resolver = Some("host".to_string());
        let s = MachineSetup::from_raw(&raw).unwrap();
        let saved = s.to_raw();
        assert_eq!(saved.hostsocket.net.as_deref(), Some("nat"));
        assert_eq!(saved.hostsocket.dns_server.as_deref(), Some("192.0.2.53"));
        assert_eq!(saved.hostsocket.hostname.as_deref(), Some("boing"));
        assert_eq!(saved.hostsocket.address.as_deref(), Some("192.168.1.50/24"));
        assert_eq!(saved.hostsocket.gateway.as_deref(), Some("192.168.1.1"));
        assert_eq!(saved.hostsocket.resolver.as_deref(), Some("host"));
    }

    #[test]
    fn a2065_nat_from_a_config_warns_only_where_the_nat_can_come_up() {
        // A loaded config can name NAT even in a build whose picker skips it;
        // there make_backend leaves the NIC isolated (deterministic), so the
        // warning must track NAT_AVAILABLE, not just the selected backend.
        let mut raw = RawConfig::default();
        raw.a2065.net = Some("nat".to_string());
        let s = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(s.value_label(LauncherField::Ethernet), "NAT");
        assert_eq!(s.ethernet_breaks_determinism(), crate::net::NAT_AVAILABLE);
    }

    #[test]
    fn a2065_bridge_interface_picker_round_trips() {
        let mut raw = RawConfig::default();
        raw.a2065.net = Some("bridge".to_string());
        raw.a2065.interface = Some("en-test".to_string());
        let mut setup = MachineSetup::from_raw(&raw).unwrap();
        setup.bridge_interfaces = vec![
            ("en-test".to_string(), "Ethernet A (en-test)".to_string()),
            ("en-next".to_string(), "Ethernet B (en-next)".to_string()),
        ];
        assert_eq!(setup.value_label(F::Ethernet), "Bridged");
        assert_eq!(
            setup.value_label(F::EthernetInterface),
            "Ethernet A (en-test)"
        );
        assert!(!setup.row_hidden(F::EthernetInterface));
        assert_eq!(
            setup.ethernet_breaks_determinism(),
            crate::net::BRIDGE_AVAILABLE
        );

        setup.cycle(F::EthernetInterface, true);
        let saved = setup.to_raw();
        assert_eq!(saved.a2065.net.as_deref(), Some("bridge"));
        assert_eq!(saved.a2065.interface.as_deref(), Some("en-next"));
        let restored = MachineSetup::from_raw(&saved).unwrap();
        assert_eq!(
            restored.a2065_net,
            Some(NetConfig::Bridge {
                interface: "en-next".to_string()
            })
        );
    }

    #[test]
    fn an_isolated_a2065_round_trips_as_net_none() {
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::Ethernet, true);
        let raw = s.to_raw();
        // Fitted-but-isolated is `net = "none"`; not fitted is an absent key.
        assert_eq!(raw.a2065.net.as_deref(), Some("none"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.a2065_net, Some(NetConfig::None));
        assert_eq!(back.value_label(LauncherField::Ethernet), "Isolated");
    }

    #[test]
    fn parallel_sampler_rows_appear_only_when_selected() {
        let has = |device| {
            rows(
                LauncherTab::IoParallel,
                device,
                SerialMode::default(),
                false,
                false,
            )
            .iter()
            .any(|r| r.field == LauncherField::SamplerInput)
        };
        // The sampler rows are hidden (not greyed) unless the sampler is chosen.
        assert!(!has(ParallelDevice::None));
        assert!(!has(ParallelDevice::Printer));
        assert!(has(ParallelDevice::Sampler));
    }

    #[test]
    fn midi_rows_appear_only_in_midi_mode() {
        let has = |mode| {
            rows(
                LauncherTab::IoPorts,
                ParallelDevice::None,
                mode,
                false,
                false,
            )
            .iter()
            .any(|r| r.field == LauncherField::MidiOut)
        };
        assert!(!has(SerialMode::Stdout));
        assert!(has(SerialMode::Midi));
    }

    #[test]
    fn parallel_device_cycles_none_printer_sampler() {
        let mut s = MachineSetup::default();
        assert_eq!(s.parallel_device, ParallelDevice::None);
        s.cycle(LauncherField::ParallelDevice, true);
        assert_eq!(s.parallel_device, ParallelDevice::Printer);
        s.cycle(LauncherField::ParallelDevice, true);
        assert_eq!(s.parallel_device, ParallelDevice::Sampler);
        s.cycle(LauncherField::ParallelDevice, true);
        assert_eq!(s.parallel_device, ParallelDevice::None);
    }

    #[test]
    fn parallel_printer_output_row_appears_and_round_trips() {
        let mut s = MachineSetup::default();
        // The Output file row shows only when the printer is selected.
        let has_output = |device| {
            rows(
                LauncherTab::IoParallel,
                device,
                SerialMode::default(),
                false,
                false,
            )
            .iter()
            .any(|r| r.field == LauncherField::ParallelOutput)
        };
        assert!(!has_output(ParallelDevice::None));
        assert!(has_output(ParallelDevice::Printer));

        s.parallel_device = ParallelDevice::Printer;
        // A printer with no capture file yet is not persisted (incomplete).
        assert_eq!(s.to_raw().parallel.device, None);

        s.set_path(LauncherField::ParallelOutput, "captures/out.prn".into());
        let raw = s.to_raw();
        assert_eq!(raw.parallel.device.as_deref(), Some("printer"));
        assert_eq!(raw.parallel.output.as_deref(), Some("captures/out.prn"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.parallel_device, ParallelDevice::Printer);
        assert_eq!(
            back.parallel_output.as_deref(),
            Some(std::path::Path::new("captures/out.prn"))
        );
    }

    #[test]
    fn parallel_sampler_selection_round_trips_through_raw() {
        let mut s = MachineSetup {
            parallel_device: ParallelDevice::Sampler,
            sampler_input: Some("BlackHole".into()),
            sampler_gain_db: 6.0,
            ..MachineSetup::default()
        };
        let raw = s.to_raw();
        assert_eq!(raw.parallel.device.as_deref(), Some("sampler"));
        assert_eq!(raw.parallel.sampler_input.as_deref(), Some("BlackHole"));
        assert_eq!(raw.parallel.sampler_gain, Some(6.0));

        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.parallel_device, ParallelDevice::Sampler);
        assert_eq!(back.sampler_input.as_deref(), Some("BlackHole"));
        assert_eq!(back.sampler_gain_db, 6.0);

        // Switching the device to None must still carry the sampler settings
        // through a Save (they do not imply the sampler on reload).
        s.parallel_device = ParallelDevice::None;
        let raw = s.to_raw();
        assert_eq!(raw.parallel.device, None);
        assert_eq!(raw.parallel.sampler_input.as_deref(), Some("BlackHole"));
        assert_eq!(raw.parallel.sampler_gain, Some(6.0));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.parallel_device, ParallelDevice::None);
        assert_eq!(back.sampler_input.as_deref(), Some("BlackHole"));
        assert_eq!(back.sampler_gain_db, 6.0);
    }

    #[test]
    fn joystick_input_mode_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Default is Gamepad, which emits no [input] section.
        assert_eq!(s.joystick_input_mode, JoystickInputMode::Gamepad);
        assert!(s.to_raw().input.joystick.is_none());
        // The stepper flips between the two explicit modes.
        s.cycle(LauncherField::Joystick, true);
        assert_eq!(s.joystick_input_mode, JoystickInputMode::Keyboard);
        let raw = s.to_raw();
        assert_eq!(raw.input.joystick.as_deref(), Some("keyboard"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.joystick_input_mode, JoystickInputMode::Keyboard);
        s.cycle(LauncherField::Joystick, true);
        assert_eq!(s.joystick_input_mode, JoystickInputMode::Gamepad);
        // Switching machine profile resets it to the Gamepad default.
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::Joystick, true);
        s.select_model(Some(MachineModel::A1200));
        assert_eq!(s.joystick_input_mode, JoystickInputMode::Gamepad);
    }

    #[test]
    fn input_routing_summary_names_the_driving_source_per_port() {
        let mut s = MachineSetup::default();
        // Stock wiring, gamepad mode.
        let lines = s.input_routing_summary();
        assert!(lines[0].contains("host mouse"), "{lines:?}");
        assert!(lines[1].contains("gamepad"), "{lines:?}");

        // Stock wiring, keyboard mode: the cursor keys take the joystick.
        s.cycle(LauncherField::Joystick, true);
        let lines = s.input_routing_summary();
        assert!(lines[1].contains("cursor keys"), "{lines:?}");

        // Two joysticks: the numpad stand-in is called out.
        s.port_devices = [PortDevice::Joystick, PortDevice::Joystick];
        let lines = s.input_routing_summary();
        assert!(lines.iter().any(|l| l.contains("numpad")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("cursor keys")), "{lines:?}");

        // Two mice, keyboard mode: the second mouse is keyboard-driven.
        s.port_devices = [PortDevice::Mouse, PortDevice::Mouse];
        let lines = s.input_routing_summary();
        assert!(lines[0].contains("host mouse"), "{lines:?}");
        assert!(lines[1].contains("as a mouse"), "{lines:?}");

        // Two mice, gamepad mode: the second mouse is undriven, with the
        // remedy named.
        s.cycle(LauncherField::Joystick, true);
        let lines = s.input_routing_summary();
        assert!(
            lines[1].contains("flip Joystick input to keyboard"),
            "{lines:?}"
        );

        // Analogue and empty ports say how (or that nothing) drives them.
        s.port_devices = [PortDevice::Analogue, PortDevice::None];
        let lines = s.input_routing_summary();
        assert!(lines[0].contains("pot-after"), "{lines:?}");
        assert!(lines[1].contains("empty"), "{lines:?}");

        // Every device/mode combination fits the settings pane (the panel
        // draws these at 8 px per character with no wrapping).
        let all = [
            PortDevice::Mouse,
            PortDevice::Joystick,
            PortDevice::Cd32Pad,
            PortDevice::Analogue,
            PortDevice::None,
        ];
        for p0 in all {
            for p1 in all {
                for _ in 0..2 {
                    s.cycle(LauncherField::Joystick, true);
                    s.port_devices = [p0, p1];
                    for line in s.input_routing_summary() {
                        assert!(
                            line.chars().count() <= 68,
                            "summary line too wide for the pane: {line:?}"
                        );
                    }
                }
            }
        }
    }

    /// Motherboard RAM tracks both of its hardware constraints: the big-box
    /// model gate and the CPU's address reach. Downgrading the CPU to a
    /// 24-bit part drops the profile-default bank (not just the row's
    /// editability) so the emitted config still validates, and the greyed
    /// row names whichever constraint bit.
    #[test]
    fn motherboard_ram_follows_model_and_cpu_gates() {
        let mut s = MachineSetup::default();
        assert_eq!(
            s.disabled_reason(LauncherField::MbRam),
            Some("needs A3000/A4000")
        );
        s.select_model(Some(MachineModel::A3000));
        assert!(s.applies(LauncherField::MbRam));
        assert_eq!(s.mb_ram, 4 * 1024 * 1024);
        while s.cpu != CpuModel::M68000 {
            s.cycle(LauncherField::Cpu, true);
        }
        assert_eq!(s.mb_ram, 0);
        assert_eq!(
            s.disabled_reason(LauncherField::MbRam),
            Some("needs 32-bit CPU")
        );
        // The profile's Zorro III RTG card is beyond a 24-bit bus too. The RTG
        // selector remains live because Picasso II/II+ are still valid choices.
        assert_eq!(s.rtg, RtgCard::None);
        assert_eq!(s.disabled_reason(LauncherField::Rtg), None);
        // The raw config overrides the profile default back to zero, so
        // this machine still launches.
        assert_eq!(s.to_raw().memory.motherboard.as_deref(), Some("0"));
        s.build_config()
            .expect("68000 A3000 with no mb RAM validates");
    }

    /// Only the A4000 cycles past Ramsey's 16M four-bank maximum into the
    /// $04000000-$06FFFFFF expansion presets; the A3000 wraps back to zero.
    #[test]
    fn motherboard_ram_expansion_presets_are_a4000_only() {
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A4000));
        while s.mb_ram != 16 * 1024 * 1024 {
            s.cycle(LauncherField::MbRam, true);
        }
        s.cycle(LauncherField::MbRam, true);
        assert_eq!(s.mb_ram, 32 * 1024 * 1024);
        s.cycle(LauncherField::MbRam, true);
        assert_eq!(s.mb_ram, 64 * 1024 * 1024);
        s.build_config().expect("64M A4000 motherboard validates");

        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A3000));
        while s.mb_ram != 16 * 1024 * 1024 {
            s.cycle(LauncherField::MbRam, true);
        }
        s.cycle(LauncherField::MbRam, true);
        assert_eq!(s.mb_ram, 0);
    }

    /// Accelerator RAM follows only the CPU's address reach: any 32-bit
    /// machine can carry it, and downgrading the CPU to a 24-bit part drops
    /// the bank so the emitted config still validates.
    #[test]
    fn accelerator_ram_follows_the_cpu_gate() {
        let mut s = MachineSetup::default();
        // The default machine is a 68000 A500: greyed out.
        assert_eq!(
            s.disabled_reason(LauncherField::AccelRam),
            Some("needs 32-bit CPU")
        );
        s.select_model(Some(MachineModel::A1200));
        while !cpu_is_32bit(s.cpu) {
            s.cycle(LauncherField::Cpu, true);
        }
        assert!(s.applies(LauncherField::AccelRam));
        s.cycle(LauncherField::AccelRam, true);
        assert_eq!(s.accel_ram, 16 * 1024 * 1024);
        assert_eq!(s.to_raw().memory.accelerator.as_deref(), Some("16M"));
        s.build_config()
            .expect("32-bit A1200 with accelerator RAM validates");
        while s.cpu != CpuModel::M68EC020 {
            s.cycle(LauncherField::Cpu, true);
        }
        assert_eq!(s.accel_ram, 0);
        assert_eq!(
            s.disabled_reason(LauncherField::AccelRam),
            Some("needs 32-bit CPU")
        );
    }

    #[test]
    fn port_devices_round_trip_through_raw_against_the_profile_baseline() {
        let mut s = MachineSetup::default();
        // Stock wiring emits no port keys.
        assert_eq!(s.port_devices, [PortDevice::Mouse, PortDevice::Joystick]);
        let raw = s.to_raw();
        assert!(raw.input.port1.is_none());
        assert!(raw.input.port2.is_none());

        // Non-default devices are written and read back.
        s.cycle(LauncherField::Port1Device, true); // Mouse -> Joystick
        s.cycle(LauncherField::Port2Device, true); // Joystick -> Cd32Pad
        let raw = s.to_raw();
        assert_eq!(raw.input.port1.as_deref(), Some("joystick"));
        assert_eq!(raw.input.port2.as_deref(), Some("cd32"));
        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(
            back.port_devices,
            [PortDevice::Joystick, PortDevice::Cd32Pad]
        );

        // The CD32 profile's bundled pad is its baseline: selecting the
        // model adopts it and keeps it implicit in the raw config.
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::Cd32));
        assert_eq!(s.port_devices[1], PortDevice::Cd32Pad);
        assert!(s.to_raw().input.port2.is_none());
    }

    #[test]
    fn build_config_surfaces_validation_errors() {
        // Z3 RAM on a 68000 (24-bit bus) is rejected by the config validator;
        // the model leans on that rather than re-checking.
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::Z3Ram, true);
        assert_eq!(s.z3_ram, 16 * 1024 * 1024);
        let err = s.build_config().unwrap_err().to_string();
        assert!(err.contains("Zorro III"), "{err}");
    }

    #[cfg(feature = "midi")]
    #[test]
    fn serial_midi_settings_round_trip_through_raw() {
        let mut s = MachineSetup::default();
        // Default serial mode writes nothing.
        assert!(s.to_raw().serial.mode.is_none());

        s.cycle(LauncherField::SerialMode, true); // Stdout -> MIDI
        assert_eq!(s.serial_mode, SerialMode::Midi);
        s.midi_out = Some("USB MIDI".to_string());

        let raw = s.to_raw();
        assert_eq!(raw.serial.mode.as_deref(), Some("midi"));
        assert_eq!(raw.serial.midi_out.as_deref(), Some("USB MIDI"));

        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.serial_mode, SerialMode::Midi);
        assert_eq!(back.midi_out.as_deref(), Some("USB MIDI"));
    }

    #[test]
    fn shader_cycles_the_presets_and_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Off is the baseline, so nothing is written for it.
        assert_eq!(s.value_label(LauncherField::Shader), "Disabled");
        assert_eq!(s.to_raw().display.shader, None);

        // With no user shader configured the picker offers the presets only,
        // and wraps straight back to Off.
        for expected in ["Scanlines", "Mask", "CRT (1084)", "Disabled"] {
            s.cycle(LauncherField::Shader, true);
            assert_eq!(s.value_label(LauncherField::Shader), expected);
        }
        // Backwards from Off lands on the last preset, not on Custom.
        s.cycle(LauncherField::Shader, false);
        assert_eq!(s.shader, ShaderMode::Crt);

        s.cycle(LauncherField::Shader, true);
        s.cycle(LauncherField::Shader, true);
        assert_eq!(s.shader, ShaderMode::Scanlines);
        // The canonical name is written, not the menu's "off" spelling.
        let raw = s.to_raw();
        assert_eq!(raw.display.shader.as_deref(), Some("scanlines"));
        assert_eq!(
            MachineSetup::from_raw(&raw).unwrap().shader,
            ShaderMode::Scanlines
        );
        assert_eq!(
            s.build_config().expect("valid config").shader,
            ShaderMode::Scanlines
        );

        // Switching machine profile returns it to the profile default.
        s.select_model(Some(MachineModel::A1200));
        assert_eq!(s.shader, ShaderMode::None);
    }

    #[test]
    fn a_configured_user_shader_stays_in_the_shader_cycle() {
        let raw = RawConfig {
            display: crate::config::RawDisplay {
                shader: Some("shaders/Aperture.wgsl".to_string()),
                ..Default::default()
            },
            ..RawConfig::default()
        };
        let mut s = MachineSetup::from_raw(&raw).unwrap();
        let custom = ShaderMode::Custom(PathBuf::from("shaders/Aperture.wgsl"));
        assert_eq!(s.shader, custom);
        assert_eq!(s.value_label(LauncherField::Shader), "Custom");
        // The path is written back verbatim, since host paths are
        // case-sensitive.
        assert_eq!(
            s.to_raw().display.shader.as_deref(),
            Some("shaders/Aperture.wgsl")
        );

        // Custom joins the cycle after the last preset, and cycling away and
        // back keeps its path.
        s.cycle(LauncherField::Shader, true);
        assert_eq!(s.shader, ShaderMode::None);
        for _ in 0..4 {
            s.cycle(LauncherField::Shader, true);
        }
        assert_eq!(s.shader, custom);

        // Selecting a preset drops the custom shader from the config file,
        // but not from the picker.
        s.cycle(LauncherField::Shader, true);
        s.cycle(LauncherField::Shader, true);
        assert_eq!(s.shader, ShaderMode::Scanlines);
        assert_eq!(s.to_raw().display.shader.as_deref(), Some("scanlines"));
        s.cycle(LauncherField::Shader, false);
        assert_eq!(s.shader, ShaderMode::None);
        s.cycle(LauncherField::Shader, false);
        assert_eq!(s.shader, custom);
    }

    /// The shader name is spelled out in six places (the parser, the picker
    /// labels, this writer, the menu label, and the two docs tables), so pin
    /// the one that matters: whatever `to_raw` writes has to load back as
    /// the same mode.
    #[test]
    fn every_shader_name_parses_back_to_its_own_mode() {
        for mode in [
            ShaderMode::None,
            ShaderMode::Scanlines,
            ShaderMode::Mask,
            ShaderMode::Crt,
            ShaderMode::Custom(PathBuf::from("shaders/Aperture.wgsl")),
        ] {
            let name = shader_name(&mode);
            assert_eq!(
                crate::config::parse_shader(&name).expect("shader name must parse"),
                mode,
                "shader_name({mode:?}) wrote {name:?}, which does not load back"
            );
        }
    }

    #[test]
    fn shader_strength_steps_in_tenths_and_clamps() {
        let mut s = MachineSetup::default();
        // Full effect is the baseline, so nothing is written for it.
        assert_eq!(s.value_label(LauncherField::ShaderStrength), "1.00");
        assert_eq!(s.to_raw().display.shader_strength, None);

        s.cycle(LauncherField::ShaderStrength, false);
        assert_eq!(s.value_label(LauncherField::ShaderStrength), "0.90");
        assert_eq!(s.to_raw().display.shader_strength, Some(0.9));

        // Both ends saturate rather than wrapping, and stepping stays on the
        // 0.1 grid instead of drifting.
        for _ in 0..20 {
            s.cycle(LauncherField::ShaderStrength, false);
        }
        assert_eq!(s.shader_strength, 0.0);
        for _ in 0..20 {
            s.cycle(LauncherField::ShaderStrength, true);
        }
        assert_eq!(s.shader_strength, 1.0);
        assert_eq!(s.to_raw().display.shader_strength, None);

        s.cycle(LauncherField::ShaderStrength, false);
        assert_eq!(s.build_config().expect("valid config").shader_strength, 0.9);
        // Switching machine profile returns it to the profile default.
        s.select_model(Some(MachineModel::A1200));
        assert_eq!(s.shader_strength, 1.0);
    }

    #[test]
    fn stereo_separation_cycles_up_on_right_and_greys_out_in_mono() {
        let mut s = MachineSetup::default();
        assert_eq!(s.audio_stereo_separation, 100);
        assert_eq!(
            s.disabled_reason(LauncherField::AudioStereoSeparation),
            None
        );

        // Right arrow (forward) steps up in 10s, wrapping 100 -> 0 -> 10.
        s.cycle(LauncherField::AudioStereoSeparation, true);
        assert_eq!(s.audio_stereo_separation, 0);
        s.cycle(LauncherField::AudioStereoSeparation, true);
        assert_eq!(s.audio_stereo_separation, 10);

        // Left arrow (backward) from 100 steps down to 90.
        let mut s = MachineSetup::default();
        s.cycle(LauncherField::AudioStereoSeparation, false);
        assert_eq!(s.audio_stereo_separation, 90);

        // Once the output is mono, separation is greyed out.
        s.cycle(LauncherField::AudioChannelMode, true);
        assert_eq!(s.audio_channel_mode, ChannelMode::Mono);
        assert_eq!(
            s.disabled_reason(LauncherField::AudioStereoSeparation),
            Some("mono")
        );
    }

    #[test]
    fn display_start_toggles_round_trip_through_raw_config() {
        let mut s = MachineSetup::default();
        // Defaults (windowed, status bar shown) emit nothing.
        let raw = s.to_raw();
        assert_eq!(raw.display.full_screen, None);
        assert_eq!(raw.display.status_bar, None);
        assert!(!s.toggle_value(LauncherField::StartFullscreen));
        assert!(s.toggle_value(LauncherField::ShowStatusBar));

        // Flip both; the non-default values now persist to [display].
        s.cycle(LauncherField::StartFullscreen, true);
        s.cycle(LauncherField::ShowStatusBar, true);
        let raw = s.to_raw();
        assert_eq!(raw.display.full_screen, Some(true));
        assert_eq!(raw.display.status_bar, Some(false));
    }

    #[test]
    fn disabled_audio_greys_out_channel_mode_and_separation() {
        use crate::audio::AudioOutput;
        let mut s = MachineSetup::default();
        // Enabled: channel mode, filter, and separation are all active.
        assert_eq!(s.disabled_reason(LauncherField::AudioChannelMode), None);
        assert_eq!(s.disabled_reason(LauncherField::AudioFilter), None);
        assert_eq!(
            s.disabled_reason(LauncherField::AudioStereoSeparation),
            None
        );

        // Disabled audio greys the shaping controls.
        s.audio_output = AudioOutput::Disabled;
        assert_eq!(
            s.disabled_reason(LauncherField::AudioChannelMode),
            Some("off")
        );
        assert_eq!(s.disabled_reason(LauncherField::AudioFilter), Some("off"));
        assert_eq!(
            s.disabled_reason(LauncherField::AudioStereoSeparation),
            Some("off")
        );
    }

    #[test]
    fn audio_output_disabled_round_trips_through_raw_config() {
        use crate::audio::AudioOutput;
        let mut s = MachineSetup::default();
        // Default is the resolved default, so it emits nothing.
        assert_eq!(s.value_label(LauncherField::AudioDevice), "Default");
        let raw = s.to_raw();
        assert_eq!(raw.audio.output_enabled, None);
        assert_eq!(raw.audio.output_device, None);

        // "Disabled" persists as output_enabled = false, no device.
        s.audio_output = AudioOutput::Disabled;
        assert_eq!(s.value_label(LauncherField::AudioDevice), "Disabled");
        let raw = s.to_raw();
        assert_eq!(raw.audio.output_enabled, Some(false));
        assert_eq!(raw.audio.output_device, None);

        // A named device persists as output_device, with output_enabled omitted.
        s.audio_output = AudioOutput::Device("BlackHole".to_string());
        let raw = s.to_raw();
        assert_eq!(raw.audio.output_device.as_deref(), Some("BlackHole"));
        assert_eq!(raw.audio.output_enabled, None);
    }

    #[test]
    fn stem_granularity_survives_a_launcher_load_and_save() {
        // No launcher row edits [audio] stem_granularity (it is a
        // headless-only default), so a loaded config's value must pass
        // through to_raw untouched rather than being dropped on Save.
        let mut raw = RawConfig::default();
        raw.audio.stem_granularity = Some("master,channel".to_string());
        let setup = MachineSetup::from_raw(&raw).expect("config loads");
        assert_eq!(
            setup.to_raw().audio.stem_granularity.as_deref(),
            Some("master,channel")
        );
        // And a config that never set it keeps not setting it.
        let bare = MachineSetup::from_raw(&RawConfig::default()).expect("config loads");
        assert_eq!(bare.to_raw().audio.stem_granularity, None);
    }

    #[cfg(feature = "midi")]
    #[test]
    fn midi_device_rows_are_hidden_off_midi_mode() {
        // Off MIDI mode the endpoint rows are absent from the Serial section
        // (they are hidden, not greyed), so it shows only the Device / Mode row.
        let serial = rows(
            LauncherTab::IoPorts,
            ParallelDevice::None,
            SerialMode::Stdout,
            false,
            false,
        );
        assert!(!serial.iter().any(|r| r.field == LauncherField::MidiOut));
        assert!(!serial.iter().any(|r| r.field == LauncherField::MidiIn));
    }

    #[test]
    fn setting_a_floppy_path_round_trips_and_wires_the_drive() {
        let mut s = MachineSetup::default();
        s.set_path(LauncherField::Df1Image, PathBuf::from("/disks/b.adf"));
        assert!(s.floppy_drives >= 2, "DF1 media wires in a second drive");
        let raw = s.to_raw();
        assert_eq!(raw.floppy.drives, Some(2));
        assert_eq!(
            raw.floppy.df1.as_ref().and_then(|d| d.path.as_deref()),
            Some("/disks/b.adf")
        );
    }

    #[test]
    fn drive_volume_name_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A1200)); // Gayle, so IDE applies.
        s.set_path(LauncherField::IdeMaster, PathBuf::from("/host/games"));
        s.set_drive_name(LauncherField::IdeMaster, "Games".to_string());
        assert_eq!(s.drive_name(LauncherField::IdeMaster), Some("Games"));

        let raw = s.to_raw();
        let master = raw.ide.master.as_ref().expect("master emitted");
        assert_eq!(master.path, "/host/games");
        assert_eq!(master.name.as_deref(), Some("Games"));

        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(back.drive_name(LauncherField::IdeMaster), Some("Games"));
    }

    #[test]
    fn drive_volume_name_without_an_image_is_dropped() {
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A1200));
        // No image set: a name has nothing to label.
        s.set_drive_name(LauncherField::IdeMaster, "Orphan".to_string());
        assert_eq!(s.drive_name(LauncherField::IdeMaster), None);

        // With an image the name sticks, then clearing the image drops it too.
        s.set_path(LauncherField::IdeMaster, PathBuf::from("/host/games"));
        s.set_drive_name(LauncherField::IdeMaster, "Games".to_string());
        assert_eq!(s.drive_name(LauncherField::IdeMaster), Some("Games"));
        s.clear_path(LauncherField::IdeMaster);
        assert_eq!(s.drive_name(LauncherField::IdeMaster), None);
    }

    #[test]
    fn drive_filesystem_round_trips_through_raw_for_a_directory_mount() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-launcher-fs-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A1200)); // Gayle, so IDE applies.
        s.set_path(LauncherField::IdeMaster, dir.clone());
        // FFS by default, and an FFS default is not written out at all.
        assert_eq!(
            s.drive_filesystem(LauncherField::IdeMaster),
            crate::diskimage::FileSystem::FFS
        );
        let raw = s.to_raw();
        assert_eq!(raw.ide.master.as_ref().unwrap().filesystem, None);

        s.cycle_drive_filesystem(LauncherField::IdeMaster);
        assert_eq!(
            s.drive_filesystem(LauncherField::IdeMaster),
            crate::diskimage::FileSystem::OFS
        );
        let raw = s.to_raw();
        assert_eq!(
            raw.ide.master.as_ref().unwrap().filesystem.as_deref(),
            Some("ofs")
        );

        let back = MachineSetup::from_raw(&raw).unwrap();
        assert_eq!(
            back.drive_filesystem(LauncherField::IdeMaster),
            crate::diskimage::FileSystem::OFS
        );

        // Toggling back to FFS and clearing the path both drop it again.
        s.cycle_drive_filesystem(LauncherField::IdeMaster);
        assert_eq!(
            s.to_raw().ide.master.as_ref().unwrap().filesystem,
            None,
            "FFS is the default and is not written out"
        );
        s.cycle_drive_filesystem(LauncherField::IdeMaster);
        s.clear_path(LauncherField::IdeMaster);
        assert_eq!(
            s.drive_filesystem(LauncherField::IdeMaster),
            crate::diskimage::FileSystem::FFS,
            "clearing the image resets the filesystem choice, like the volume name"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn drive_filesystem_is_not_emitted_for_a_non_directory_path() {
        // A raw HDF/image path already carries its own filesystem; even if
        // the field is toggled (e.g. leftover session state from an earlier
        // directory at the same slot), `to_raw` must never write a
        // `filesystem` key config.rs would then reject at parse time.
        let mut s = MachineSetup::default();
        s.select_model(Some(MachineModel::A1200));
        s.set_path(LauncherField::IdeMaster, PathBuf::from("/host/games.hdf"));
        s.cycle_drive_filesystem(LauncherField::IdeMaster);
        assert_eq!(
            s.drive_filesystem(LauncherField::IdeMaster),
            crate::diskimage::FileSystem::OFS
        );
        let raw = s.to_raw();
        assert_eq!(raw.ide.master.as_ref().unwrap().filesystem, None);
    }

    #[test]
    fn editing_a_drive_name_commits_to_the_setup() {
        let mut setup = MachineSetup::default();
        setup.select_model(Some(MachineModel::A1200));
        setup.set_path(LauncherField::ScsiUnit0, PathBuf::from("/host/work"));
        let mut state = LauncherState::new(setup);
        state.begin_edit_drive_name(LauncherField::ScsiUnit0);
        for ch in "WORK".chars() {
            state.edit_push(ch);
        }
        state.edit_commit();
        assert_eq!(state.editing(), None);
        assert_eq!(
            state.setup.drive_name(LauncherField::ScsiUnit0),
            Some("WORK")
        );
    }
}
