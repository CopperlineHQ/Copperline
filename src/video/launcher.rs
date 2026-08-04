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
    format_size, machine_profile_defaults, AudioFilterMode, BridgeCable, BridgeDensity,
    BridgeDriver, BridgeSpeedMode, ChannelMode, Chipset, Config, CpuModel, DisplayScaling,
    FloppyBridgeConfig, JoystickInputMode, MachineModel, MenuScale, MouseCapture, Mt32Lcd,
    Overscan, PacingBudget, ParallelDevice, PixelAspect, RawConfig, RawDrive, RawFilesysMount,
    RawFloppyDrive, RawZorroBoard, RtgCard, ScsiController, SerialMode, ShaderMode, Tint,
    WarpSpeed, BOOT_PRI_NEVER,
};
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
    /// FloppyBridge settings for one bay, reached from its Configure button.
    FloppyBridge,
    BootPriority,
    HostFs,
    Cd,
    /// Serial and parallel ports on one tab, under `Serial:` / `Parallel:`
    /// section headings.
    IoPorts,
    Input,
    Zorro,
    /// The "A/V & Emu" strip tab, whose default category is Audio (its rows are
    /// the audio settings). Video and Emulation are its sibling categories,
    /// switched between via the top nav row, with no Back button.
    /// `AvAudio.label()` is therefore the strip's "A/V & Emu", not "Audio".
    AvAudio,
    AvVideo,
    AvEmulation,
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

impl LauncherTab {
    pub fn label(self) -> &'static str {
        match self {
            LauncherTab::System => "System",
            LauncherTab::Cpu => "CPU",
            LauncherTab::Memory => "Memory",
            LauncherTab::Rom => "ROM",
            LauncherTab::Floppy => "Floppy",
            LauncherTab::FloppyBridge => "FloppyBridge",
            LauncherTab::Storage => "Storage",
            LauncherTab::BootPriority => "Boot Priority",
            LauncherTab::HostFs => "Host Mounts",
            LauncherTab::Cd => "CD",
            LauncherTab::IoPorts => "I/O Ports",
            LauncherTab::Input => "Input",
            LauncherTab::Zorro => "Zorro",
            LauncherTab::AvAudio => "A/V & Emu",
            LauncherTab::AvVideo => "Video",
            LauncherTab::AvEmulation => "Emulation",
        }
    }

    /// The strip entry to highlight for this (possibly sub-page) tab: the Storage
    /// sub-pages keep the Storage strip entry lit, and the A/V categories keep
    /// the A/V & Emu one.
    pub fn strip_tab(self) -> LauncherTab {
        match self {
            LauncherTab::Cd | LauncherTab::HostFs | LauncherTab::BootPriority => {
                LauncherTab::Storage
            }
            LauncherTab::FloppyBridge => LauncherTab::Floppy,
            LauncherTab::AvVideo | LauncherTab::AvEmulation => LauncherTab::AvAudio,
            other => other,
        }
    }

    /// The parent tab a sub-page returns to via its Back button, or `None` when
    /// the page has no Back (the A/V categories switch between each other via the
    /// top nav row instead).
    pub fn parent_tab(self) -> Option<LauncherTab> {
        match self {
            LauncherTab::Cd | LauncherTab::HostFs | LauncherTab::BootPriority => {
                Some(LauncherTab::Storage)
            }
            LauncherTab::FloppyBridge => Some(LauncherTab::Floppy),
            _ => None,
        }
    }

    /// The top nav row's buttons -- sibling pages reachable from here as
    /// `(label, tab)` pairs. Empty when the page shows a Back button instead.
    /// The button whose tab is the current page is drawn highlighted.
    pub fn nav_options(self) -> &'static [(&'static str, LauncherTab)] {
        match self {
            LauncherTab::Storage => STORAGE_NAV,
            LauncherTab::AvAudio | LauncherTab::AvVideo | LauncherTab::AvEmulation => AV_NAV,
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
    ("CD", LauncherTab::Cd),
    ("Host Mounts", LauncherTab::HostFs),
    ("Boot Priority", LauncherTab::BootPriority),
];

/// The A/V & Emu categories, left to right (matching "A/V"). `AvAudio` is the
/// default, so its button reads "Audio".
const AV_NAV: &[(&str, LauncherTab)] = &[
    ("Audio", LauncherTab::AvAudio),
    ("Video", LauncherTab::AvVideo),
    ("Emulation", LauncherTab::AvEmulation),
];

/// A single editable setting. Parameter-free variants keep the per-tab row
/// tables and `UiControl` hit-testing simple (every control is one `Copy` enum
/// value); the floppy/SCSI families are spelled out rather than indexed for the
/// same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherField {
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
    BridgeSpeed,
    BridgeServeSpeed,
    BridgeAutoCache,
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
    // Serial (MIDI). Present only with the `midi` feature.
    #[cfg(feature = "midi")]
    SerialMode,
    #[cfg(feature = "midi")]
    MidiOut,
    Mt32ControlRom,
    Mt32PcmRom,
    Mt32Panel,
    Mt32Lcd,
    #[cfg(feature = "midi")]
    MidiIn,
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
    /// A non-interactive greyed heading that groups the rows beneath it
    /// (e.g. the `Serial:` / `Parallel:` sections of the I/O Ports tab). Its
    /// `field` is inert.
    SectionHeader,
    /// The greyed `Drive` / `Priority` / `Status` column titles above the Boot
    /// Priority rows. Non-interactive; its `field` is inert.
    BootpriHeader,
    /// A floppy drive's media row: an image path with Browse/Clear, or the
    /// real interface in use with a Configure button once bridged.
    FloppyMedia,
    /// The pair of tick boxes under a drive: write protect, and whether the
    /// bay uses a real drive. Its `field` is the drive's write-protect field.
    FloppyFlags,
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
    row(F::Rtc, "Real-time clock", Toggle),
    row(F::Identify, "Identify board", Toggle),
    row(F::Rtg, "RTG card", Cycle),
];
const CPU_ROWS: [Row; 6] = [
    row(F::Cpu, "CPU", Cycle),
    row(F::Fpu, "FPU (68881/2)", Toggle),
    row(F::Clock, "Clock", Cycle),
    row(F::Icache, "Instruction cache", Toggle),
    row(F::Dcache, "Data cache", Toggle),
    row(F::Jit, "JIT accelerator", Toggle),
];
const MEMORY_ROWS: [Row; 6] = [
    row(F::ChipRam, "Chip RAM", Cycle),
    row(F::FastRam, "Fast RAM", Cycle),
    row(F::SlowRam, "Slow RAM", Cycle),
    row(F::MbRam, "Motherboard RAM", Cycle),
    row(F::AccelRam, "Accelerator RAM", Cycle),
    row(F::Z3Ram, "Zorro III RAM", Cycle),
];
const ROM_ROWS: [Row; 2] = [
    row(F::Rom, "Kickstart ROM", PathRow),
    row(F::ExtendedRom, "Extended ROM", PathRow),
];
// Each drive is a greyed "DFn:" heading with its settings indented under it. The
// heading is keyed on the drive's image field so `row_hidden` drops it along
// with the drive's rows when the drive is not wired in.
// Each wired drive is a greyed "DFn:" heading, its media row, then the two
// tick boxes that share a line beneath. The media row shows an image path with
// Browse/Clear, or -- once FloppyBridge is ticked -- the real interface in use
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
/// The FloppyBridge settings page, shown for whichever bay was configured.
#[cfg(feature = "floppybridge")]
const FLOPPY_BRIDGE_ROWS: [Row; 8] = [
    // Inert: the label is built from the loaded library's version (see
    // `bridge_library_heading`), so the text here is never drawn.
    row(F::BridgeLibrary, "", RowKind::SectionHeader),
    row(F::BridgeDevice, "Interface", Cycle),
    row(F::BridgePort, "Serial port", Cycle),
    row(F::BridgeCable, "Drive select", Cycle),
    row(F::BridgeDensity, "Density", Cycle),
    row(F::BridgeSpeed, "Read mode", Cycle),
    row(F::BridgeServeSpeed, "Bridge speed", Cycle),
    row(F::BridgeAutoCache, "Auto-cache", Toggle),
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
// The MIDI endpoint rows appear only when the serial port is in MIDI mode, so
// the Serial section shows just the Device / Mode selector otherwise. The
// selector is labelled "Device / Mode" because some choices are devices (MIDI)
// and some are modes (stdout, PTY, TCP).
// Rows under each I/O Ports section heading are indented two spaces so they
// read as belonging to their `Serial:` / `Parallel:` / `Ethernet:` port.
#[cfg(feature = "midi")]
const SERIAL_ROWS_BASE: [Row; 1] = [row(F::SerialMode, "  Device / Mode", Cycle)];
#[cfg(feature = "midi")]
const SERIAL_ROWS_MIDI: [Row; 3] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::MidiIn, "  MIDI input", Cycle),
    row(F::MidiOut, "  MIDI output", Cycle),
];
// Picking Munt MT-32 as the output adds the two ROM images it runs on and
// its front panel; nothing else needs them, so nothing else shows them.
#[cfg(all(feature = "midi", feature = "mt32"))]
const SERIAL_ROWS_MT32: [Row; 7] = [
    row(F::SerialMode, "  Device / Mode", Cycle),
    row(F::MidiIn, "  MIDI input", Cycle),
    row(F::MidiOut, "  MIDI output", Cycle),
    row(F::Mt32ControlRom, "  Control ROM", PathRow),
    row(F::Mt32PcmRom, "  PCM ROM", PathRow),
    row(F::Mt32Panel, "  Front panel", Toggle),
    row(F::Mt32Lcd, "  Display", Cycle),
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
const ETHERNET_ROWS: [Row; 2] = [
    row(F::Ethernet, "  A2065 board", Cycle),
    row(F::EthernetInterface, "  Host adapter", Cycle),
];
// The A/V & Emu tab is split into three categories switched via the top nav row.
// The Video category also carries the CRT-shader controls (a picture setting).
const VIDEO_ROWS: [Row; 13] = [
    row(F::StartFullscreen, "Start fullscreen", Toggle),
    row(F::ShowStatusBar, "Status bar", Toggle),
    row(F::Bezel, "Monitor bezel", Toggle),
    row(F::PerfOverlay, "Perf overlay", Toggle),
    row(F::MenuScale, "Menu size", Cycle),
    row(F::Overscan, "Overscan", Cycle),
    row(F::PixelAspect, "Pixel aspect", Cycle),
    row(F::Scaling, "Scaling", Cycle),
    row(F::Deinterlace, "Deinterlace", Toggle),
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
    row(F::FloppySounds, "Floppy sounds", Toggle),
    row(F::FloppyVolume, "Floppy volume", Cycle),
];
const EMULATION_ROWS: [Row; 4] = [
    row(F::PowerOn, "Power on startup", Toggle),
    row(F::RealtimePriority, "Realtime priority", Toggle),
    row(F::PacingBudget, "Pacing budget", Cycle),
    row(F::Warp, "Warp speed", Cycle),
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
) -> Cow<'static, [Row]> {
    match tab {
        LauncherTab::System => Cow::Borrowed(&SYSTEM_ROWS),
        LauncherTab::Cpu => Cow::Borrowed(&CPU_ROWS),
        LauncherTab::Memory => Cow::Borrowed(&MEMORY_ROWS),
        LauncherTab::Rom => Cow::Borrowed(&ROM_ROWS),
        LauncherTab::Floppy => Cow::Borrowed(&FLOPPY_ROWS),
        // Unreachable without the feature: nothing offers a way in, since the
        // tick box that turns a bay over is not drawn either.
        #[cfg(not(feature = "floppybridge"))]
        LauncherTab::FloppyBridge => Cow::Borrowed(&[]),
        #[cfg(feature = "floppybridge")]
        LauncherTab::FloppyBridge => Cow::Borrowed(&FLOPPY_BRIDGE_ROWS),
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
        LauncherTab::Cd => Cow::Borrowed(&CD_ROWS),
        LauncherTab::IoPorts => Cow::Owned(io_ports_rows(
            serial_mode,
            midi_out_is_mt32,
            parallel_device,
        )),
        LauncherTab::Input => Cow::Borrowed(&INPUT_ROWS),
        LauncherTab::Zorro => Cow::Borrowed(&[]),
        // A/V & Emu defaults to the Audio category; Video and Emulation are its
        // sibling categories, switched via the top nav row.
        LauncherTab::AvAudio => Cow::Borrowed(&AUDIO_ROWS),
        LauncherTab::AvVideo => Cow::Borrowed(&VIDEO_ROWS),
        LauncherTab::AvEmulation => Cow::Borrowed(&EMULATION_ROWS),
    }
}

/// The I/O Ports tab: a `Serial:` section (only in a `midi` build, which is the
/// only build with serial rows), a `Parallel:` section, then an `Ethernet:`
/// section, each under a greyed heading and each showing only the rows relevant
/// to its selected device/mode.
fn io_ports_rows(
    serial_mode: SerialMode,
    midi_out_is_mt32: bool,
    parallel_device: ParallelDevice,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let serial = serial_rows(serial_mode, midi_out_is_mt32);
    if !serial.is_empty() {
        rows.push(section_header("Serial:"));
        rows.extend_from_slice(serial);
    }
    rows.push(section_header("Parallel:"));
    rows.extend_from_slice(parallel_rows(parallel_device));
    rows.push(section_header("Ethernet:"));
    rows.extend_from_slice(&ETHERNET_ROWS);
    rows
}

/// Serial rows for the current mode. Only the `midi` build has any; without it
/// the Serial section is empty and omitted from the I/O Ports tab.
fn serial_rows(serial_mode: SerialMode, midi_out_is_mt32: bool) -> &'static [Row] {
    #[cfg(feature = "midi")]
    {
        if serial_mode != SerialMode::Midi {
            return &SERIAL_ROWS_BASE;
        }
        #[cfg(feature = "mt32")]
        if midi_out_is_mt32 {
            return &SERIAL_ROWS_MT32;
        }
        let _ = midi_out_is_mt32;
        &SERIAL_ROWS_MIDI
    }
    #[cfg(not(feature = "midi"))]
    {
        let _ = (serial_mode, midi_out_is_mt32);
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
const RTG_CARDS: [RtgCard; 4] = [
    RtgCard::None,
    RtgCard::Picasso2,
    RtgCard::Picasso2Plus,
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
/// How close a bay is to actually having a physical drive behind it.
///
/// The two ways it can fail look identical from the outside but need opposite
/// fixes -- install the library, or plug the interface in -- so the launcher
/// keeps them apart rather than saying "None" to both.
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
#[cfg(feature = "floppybridge")]
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
/// devices it did not name. A macOS device counts as already named when the
/// scan lists its `tty.` twin: `cu.usbmodem101` and `tty.usbmodem101` are
/// one port, and the scan's spelling wins.
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

/// The combined port list, or the bare "Automatic" without the feature.
fn sample_bridge_ports() -> Vec<Option<String>> {
    #[cfg(feature = "floppybridge")]
    {
        merge_port_lists(crate::floppybridge::com_ports(), host_serial_ports())
    }
    #[cfg(not(feature = "floppybridge"))]
    {
        vec![None]
    }
}

/// What the bridge can see of the host right now.
fn bridge_status() -> BridgeStatus {
    #[cfg(feature = "floppybridge")]
    if crate::floppybridge::interface_connected() {
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

fn bridge_mode_name(m: BridgeSpeedMode) -> &'static str {
    match m {
        BridgeSpeedMode::Compatible => "compatible",
        BridgeSpeedMode::Normal => "normal",
        BridgeSpeedMode::Stalling => "stalling",
    }
}

const BRIDGE_DRIVERS: [BridgeDriver; 3] = [
    BridgeDriver::DrawBridge,
    BridgeDriver::Greaseweazle,
    BridgeDriver::SupercardPro,
];
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
const BRIDGE_SPEEDS: [BridgeSpeedMode; 3] = [
    BridgeSpeedMode::Normal,
    BridgeSpeedMode::Compatible,
    BridgeSpeedMode::Stalling,
];
const AUDIO_FILTER_MODES: [AudioFilterMode; 3] = [
    AudioFilterMode::Auto,
    AudioFilterMode::On,
    AudioFilterMode::Off,
];
const FLOPPY_SPEEDS: [u16; 5] = [100, 200, 400, 800, crate::floppy::SPEED_TURBO];
// The bridge speed row cycles the same set the config and CLI accept.
use crate::config::SUPPORTED_BRIDGE_SPEED_PERCENTS as BRIDGE_SERVE_SPEEDS;
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
    df_bridge: [Option<FloppyBridgeConfig>; 4],
    /// The bay's interface is set to "None": still a physical-drive bay in
    /// the launcher, but with no interface to drive it -- the run and the
    /// written config treat it as unbridged. Selected automatically when a
    /// bay is bridged with nothing attached.
    df_bridge_none: [bool; 4],
    /// Which bay the FloppyBridge settings page is showing. The page itself is
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
    bridge_ports: Vec<Option<String>>,
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
    ide_master_bootpri: Option<i8>,
    ide_master_boot_off: bool,
    ide_slave: Option<PathBuf>,
    ide_slave_name: Option<String>,
    ide_slave_bootpri: Option<i8>,
    ide_slave_boot_off: bool,
    /// Which SCSI host adapter is fitted, or `None` for no board. Shares the
    /// `scsi_*` ROM/unit block below (the drives are portable between boards).
    scsi_controller: Option<ScsiController>,
    scsi_rom: Option<PathBuf>,
    scsi_rom_odd: Option<PathBuf>,
    scsi_units: [Option<PathBuf>; 7],
    scsi_unit_names: [Option<String>; 7],
    scsi_unit_bootpri: [Option<i8>; 7],
    scsi_unit_boot_off: [bool; 7],
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
    // Serial port. Carried in every build so a config's `[serial]` block
    // round-trips; only edited in the I/O Ports tab's Serial section, which a
    // `midi` build shows.
    serial_mode: SerialMode,
    midi_out: Option<String>,
    midi_in: Option<String>,
    /// TCP listen address for `mode = "tcp"`; carried so the override
    /// round-trips through a launcher save even though no tab edits it.
    serial_listen: Option<String>,
    /// Dial-out address for `mode = "tcp-connect"`; carried like
    /// `serial_listen` (no tab edits it, a save must not drop it).
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
    /// Monitor-style front bezel around the picture ([display] bezel).
    bezel: bool,
    /// Performance overlay in the top-right ([display] perf_overlay).
    perf_overlay: bool,
    /// The MT-32's two ROM images, whether its front panel starts up, and
    /// how that panel's display is lit.
    mt32_control_rom: Option<PathBuf>,
    mt32_pcm_rom: Option<PathBuf>,
    mt32_panel: bool,
    mt32_lcd: Mt32Lcd,
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
            bridge_ports: sample_bridge_ports(),
            ide_master: cfg.ide.master.as_ref().map(|d| d.path.clone()),
            ide_master_name: cfg.ide.master.as_ref().and_then(|d| d.volume_name.clone()),
            ide_master_bootpri: boot_priority_of(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_master_boot_off: boot_is_off(raw.ide.master.as_ref().and_then(|d| d.bootpri)),
            ide_slave: cfg.ide.slave.as_ref().map(|d| d.path.clone()),
            ide_slave_name: cfg.ide.slave.as_ref().and_then(|d| d.volume_name.clone()),
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
            scsi_unit_bootpri: std::array::from_fn(|i| {
                boot_priority_of(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
            }),
            scsi_unit_boot_off: std::array::from_fn(|i| {
                boot_is_off(raw_scsi_unit(&raw.scsi, i).and_then(|d| d.bootpri))
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
            perf_overlay: cfg.perf_overlay,
            mt32_control_rom: cfg.serial.mt32_control_rom.clone(),
            mt32_pcm_rom: cfg.serial.mt32_pcm_rom.clone(),
            mt32_panel: cfg.serial.mt32_panel,
            mt32_lcd: cfg.serial.mt32_lcd,
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
        })
    }

    /// Load a configuration file into the typed model, validating it.
    pub fn load_from(path: &Path) -> Result<Self> {
        Self::from_raw(&crate::config::raw_from_path(path)?)
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

    pub fn serial_mode(&self) -> SerialMode {
        self.serial_mode
    }

    pub fn parallel_device(&self) -> ParallelDevice {
        self.parallel_device
    }

    /// Whether the selected Ethernet backend carries traffic on the host's
    /// schedule rather than the emulated clock, breaking byte-identical
    /// replay (the I/O Ports tab shows a warning). The loopback backend
    /// echoes frames deterministically and an isolated or absent NIC never
    /// sees traffic, so NAT and a direct adapter bridge qualify.
    pub fn ethernet_breaks_determinism(&self) -> bool {
        matches!(
            self.a2065_net.as_ref(),
            Some(NetConfig::Nat) if crate::net::NAT_AVAILABLE
        ) || matches!(
            self.a2065_net.as_ref(),
            Some(NetConfig::Bridge { .. }) if crate::net::BRIDGE_AVAILABLE
        )
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
        );
        raw.ide.slave = drive_raw(
            self.ide_slave.as_deref(),
            self.ide_slave_name.as_deref(),
            self.effective_bootpri(F::IdeSlaveBoot),
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
            );
            raw.scsi.unit1 = drive_raw(
                self.scsi_units[1].as_deref(),
                self.scsi_unit_names[1].as_deref(),
                self.effective_bootpri(F::ScsiUnit1Boot),
            );
            raw.scsi.unit2 = drive_raw(
                self.scsi_units[2].as_deref(),
                self.scsi_unit_names[2].as_deref(),
                self.effective_bootpri(F::ScsiUnit2Boot),
            );
            raw.scsi.unit3 = drive_raw(
                self.scsi_units[3].as_deref(),
                self.scsi_unit_names[3].as_deref(),
                self.effective_bootpri(F::ScsiUnit3Boot),
            );
            raw.scsi.unit4 = drive_raw(
                self.scsi_units[4].as_deref(),
                self.scsi_unit_names[4].as_deref(),
                self.effective_bootpri(F::ScsiUnit4Boot),
            );
            raw.scsi.unit5 = drive_raw(
                self.scsi_units[5].as_deref(),
                self.scsi_unit_names[5].as_deref(),
                self.effective_bootpri(F::ScsiUnit5Boot),
            );
            raw.scsi.unit6 = drive_raw(
                self.scsi_units[6].as_deref(),
                self.scsi_unit_names[6].as_deref(),
                self.effective_bootpri(F::ScsiUnit6Boot),
            );
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
        // CD
        raw.cd.image = self.cd_image.as_deref().map(path_string);
        if self.cd_insert_delay != 0.0 {
            raw.cd.insert_delay = Some(self.cd_insert_delay);
        }
        raw.cd.nvram = self.cd32_nvram.as_deref().map(path_string);
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
            raw.display.bezel = Some(self.bezel);
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
            let default = FloppyBridgeConfig::default();
            return Some(RawFloppyDrive {
                bridge: Some(bridge_driver_name(bridge.driver).to_string()),
                bridge_port: bridge.port.clone(),
                bridge_cable: (bridge.cable != default.cable)
                    .then(|| bridge_cable_name(bridge.cable).to_string()),
                bridge_density: (bridge.density != default.density)
                    .then(|| bridge_density_name(bridge.density).to_string()),
                bridge_mode: (bridge.mode != default.mode)
                    .then(|| bridge_mode_name(bridge.mode).to_string()),
                bridge_speed: (bridge.speed != crate::config::DEFAULT_BRIDGE_SPEED_PERCENT)
                    .then_some(bridge.speed),
                bridge_auto_cache: bridge.auto_cache.then_some(true),
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
            // Bridges are emitted by the FloppyBridge page, not the image rows.
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
        self.perf_overlay = base.perf_overlay;
        self.tint = base.tint;
        self.menu_scale = base.menu_scale;
        self.mt32_control_rom = base.serial.mt32_control_rom.clone();
        self.mt32_pcm_rom = base.serial.mt32_pcm_rom.clone();
        self.mt32_panel = base.serial.mt32_panel;
        self.mt32_lcd = base.serial.mt32_lcd;
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
    }

    fn has_gayle(&self) -> bool {
        matches!(self.model, Some(MachineModel::A600 | MachineModel::A1200))
    }

    fn has_sdmac(&self) -> bool {
        self.model == Some(MachineModel::A3000)
    }

    /// Machines with an IDE port: Gayle's, and the A4000's at $DD2020.
    fn has_ide(&self) -> bool {
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
            F::EthernetInterface => {
                !matches!(self.a2065_net.as_ref(), Some(NetConfig::Bridge { .. }))
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
                        .and_then(|drive| self.path(drive))
                        .is_none()
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
            // Picasso II/II+ remain available on a 24-bit CPU. The stepper's
            // choice list omits the Zorro III-only Z3660 in that case.
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
            F::IdeMaster | F::IdeSlave => reason(self.has_ide(), "needs A600/A1200/A4000"),
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
            F::CdImage | F::CdInsertDelay => reason(self.has_cd(), "needs CDTV/CD32"),
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
            | F::ScsiUnit6Boot => {
                let drive = Self::boot_field_drive(field).expect("boot field");
                match self.path(drive) {
                    None => Some("No drive"),
                    Some(p) if crate::config::is_cd_image_path(p) => Some("CD-ROM"),
                    Some(_) => None,
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
            #[cfg(feature = "floppybridge")]
            F::BridgeDevice => reason(self.bridge_edit().is_some(), "No drive"),
            #[cfg(feature = "floppybridge")]
            F::BridgeCable
            | F::BridgeDensity
            | F::BridgeSpeed
            | F::BridgeServeSpeed
            | F::BridgeAutoCache
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
            #[cfg(feature = "floppybridge")]
            F::BridgePort => {
                if self.bridge_edit().is_none() || self.df_bridge_none[self.bridge_edit_drive] {
                    Some("no interface")
                } else if self.bridge_ports.len() <= 1 {
                    Some("no ports")
                } else {
                    reason(
                        self.bridge_driver_supports(crate::floppybridge::config_option::COM_PORT),
                        "not on this interface",
                    )
                }
            }
            #[cfg(feature = "floppybridge")]
            F::BridgeCable => reason(
                self.bridge_driver_supports(crate::floppybridge::config_option::DRIVE_AB_CABLE)
                    || self.bridge_driver_supports(
                        crate::floppybridge::config_option::SUPPORTS_SHUGART,
                    ),
                "not on this interface",
            ),
            #[cfg(feature = "floppybridge")]
            F::BridgeAutoCache => reason(
                self.bridge_driver_supports(crate::floppybridge::config_option::AUTO_CACHE),
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
            F::BridgeAutoCache => self.bridge_edit().is_some_and(|c| c.auto_cache),
            F::ShowStatusBar => self.show_status_bar,
            F::Deinterlace => self.deinterlace,
            F::Bezel => self.bezel,
            F::PerfOverlay => self.perf_overlay,
            F::Mt32Panel => self.mt32_panel,
            F::PowerOn => self.power_on,
            F::RealtimePriority => self.realtime_priority,
            _ => false,
        }
    }

    /// The current path of a path field, if any.
    pub fn path(&self, field: LauncherField) -> Option<&Path> {
        match field {
            F::Rom => self.rom.as_deref(),
            F::Mt32ControlRom => self.mt32_control_rom.as_deref(),
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
            F::Filesys0Dir => self.filesys_dirs[0].as_deref(),
            F::Filesys1Dir => self.filesys_dirs[1].as_deref(),
            F::Filesys2Dir => self.filesys_dirs[2].as_deref(),
            F::Filesys3Dir => self.filesys_dirs[3].as_deref(),
            F::CdImage => self.cd_image.as_deref(),
            F::Cd32Nvram => self.cd32_nvram.as_deref(),
            F::ParallelOutput => self.parallel_output.as_deref(),
            _ => None,
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
            F::Filesys0Dir => &self.filesys_names[0],
            F::Filesys1Dir => &self.filesys_names[1],
            F::Filesys2Dir => &self.filesys_names[2],
            F::Filesys3Dir => &self.filesys_names[3],
            _ => return None,
        };
        name.as_deref()
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
    /// CD-ROM drive) has nothing to name.
    pub fn drive_name_applies(&self, field: LauncherField) -> bool {
        !self
            .path(field)
            .is_some_and(crate::config::is_cd_image_path)
    }

    pub fn value_label(&self, field: LauncherField) -> String {
        match field {
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
            F::MenuScale => self.menu_scale.menu_label().to_string(),
            F::Mt32Lcd => self.mt32_lcd.menu_label().to_string(),
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
            F::BridgeSpeed => match self.bridge_edit().map(|c| c.mode) {
                Some(BridgeSpeedMode::Compatible) => "Compatible".to_string(),
                Some(BridgeSpeedMode::Normal) => "Normal".to_string(),
                Some(BridgeSpeedMode::Stalling) => "Stalling".to_string(),
                None => "(none)".to_string(),
            },
            F::BridgeServeSpeed => {
                format!("{}%", self.bridge_edit().map_or(100, |c| c.speed))
            }
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
            #[cfg(feature = "midi")]
            F::MidiOut => {
                if self.midi_out_is_mt32() {
                    return crate::midi::MIDI_OUT_MT32_LABEL.to_string();
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
            | F::ScsiUnit6Boot => drive_bootpri_label(self.effective_bootpri(field)),
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
            // SCSI units: flag CD images, which attach a CD-ROM drive
            // rather than a hard disk at that ID.
            F::ScsiUnit0
            | F::ScsiUnit1
            | F::ScsiUnit2
            | F::ScsiUnit3
            | F::ScsiUnit4
            | F::ScsiUnit5
            | F::ScsiUnit6 => {
                let label = self.path_label(field, "(none)");
                match self.path(field) {
                    Some(p) if crate::config::is_cd_image_path(p) => format!("{label} (CD-ROM)"),
                    _ => label,
                }
            }
            // Path/drive fields: the file name, or a placeholder.
            F::Rom => self.path_label(field, "(bundled AROS)"),
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
            F::Chipset => self.chipset = cycle_slice(&CHIPSETS, self.chipset, forward),
            F::Rtg => {
                let cards = if cpu_is_32bit(self.cpu) {
                    &RTG_CARDS[..]
                } else {
                    &RTG_CARDS[..3]
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
                    // the Zorro III RTG card all sit beyond a 24-bit bus;
                    // dropping them (rather than just greying their rows)
                    // keeps the emitted config launchable. Picasso II/II+ are
                    // Zorro II cards and remain fitted.
                    self.z3_ram = 0;
                    self.mb_ram = 0;
                    self.accel_ram = 0;
                    if self.rtg == RtgCard::Z3660 {
                        self.rtg = RtgCard::None;
                    }
                }
            }
            F::Clock => self.clock_mhz = cycle_floats(&CLOCK_PRESETS, self.clock_mhz, forward),
            F::ChipRam => self.chip_ram = cycle_slice(&CHIP_PRESETS, self.chip_ram, forward),
            F::FastRam => self.fast_ram = cycle_nearest(&FAST_PRESETS, self.fast_ram, forward),
            F::SlowRam => self.slow_ram = cycle_nearest(&SLOW_PRESETS, self.slow_ram, forward),
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
                #[cfg(feature = "floppybridge")]
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
            F::MenuScale => {
                self.menu_scale = cycle_slice(&MenuScale::MENU_ORDER, self.menu_scale, forward);
            }
            F::Mt32Lcd => {
                self.mt32_lcd = cycle_slice(&Mt32Lcd::MENU_ORDER, self.mt32_lcd, forward);
            }
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
                self.scsi_controller = cycle_slice(&choices, self.scsi_controller, forward)
            }
            #[cfg(feature = "midi")]
            F::SerialMode => {
                // tcp-connect is only on offer when the config already
                // carries a dial-out address (the launcher has no editor
                // for it, and the mode fails at Run without one) -- same
                // pattern as the printer's capture path below.
                let choices: Vec<SerialMode> = SERIAL_MODES
                    .into_iter()
                    .filter(|m| self.serial_connect.is_some() || *m != SerialMode::TcpConnect)
                    .collect();
                self.serial_mode = cycle_slice(&choices, self.serial_mode, forward)
            }
            #[cfg(feature = "midi")]
            F::MidiOut => {
                // Munt MT-32 rides at the end of the output list: it is
                // always there to be chosen, whatever the host offers.
                let names: Vec<String> = self
                    .midi_endpoints
                    .outputs
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(mt32_endpoint(true))
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
                let mut choices = vec![None, Some(NetConfig::None), Some(NetConfig::Loopback)];
                if crate::net::NAT_AVAILABLE {
                    choices.push(Some(NetConfig::Nat));
                }
                if crate::net::BRIDGE_AVAILABLE {
                    let interface = match self.a2065_net.as_ref() {
                        Some(NetConfig::Bridge { interface }) => Some(interface.clone()),
                        _ => self.bridge_interfaces.first().map(|(name, _)| name.clone()),
                    };
                    if let Some(interface) = interface {
                        choices.push(Some(NetConfig::Bridge { interface }));
                    }
                }
                let current = self.a2065_net.clone();
                let index = choices
                    .iter()
                    .position(|item| *item == current)
                    .unwrap_or(0);
                let next = if forward {
                    (index + 1) % choices.len()
                } else {
                    (index + choices.len() - 1) % choices.len()
                };
                self.a2065_net = choices[next].clone();
            }
            F::EthernetInterface => {
                if let Some(NetConfig::Bridge { interface }) = self.a2065_net.as_mut() {
                    if !self.bridge_interfaces.is_empty() {
                        let index = self
                            .bridge_interfaces
                            .iter()
                            .position(|(name, _)| name == interface)
                            .unwrap_or(0);
                        let next = if forward {
                            (index + 1) % self.bridge_interfaces.len()
                        } else {
                            (index + self.bridge_interfaces.len() - 1)
                                % self.bridge_interfaces.len()
                        };
                        *interface = self.bridge_interfaces[next].0.clone();
                    }
                }
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
                let bay = self.bridge_edit_drive;
                if self.bridge_edit().is_some() {
                    if self.df_bridge_none[bay] {
                        self.df_bridge_none[bay] = false;
                        let end = if forward {
                            BRIDGE_DRIVERS[0]
                        } else {
                            BRIDGE_DRIVERS[BRIDGE_DRIVERS.len() - 1]
                        };
                        if let Some(c) = self.bridge_edit_mut() {
                            c.driver = end;
                        }
                    } else {
                        let (first, last) =
                            (BRIDGE_DRIVERS[0], BRIDGE_DRIVERS[BRIDGE_DRIVERS.len() - 1]);
                        let at_edge = self
                            .bridge_edit()
                            .is_some_and(|c| c.driver == if forward { last } else { first });
                        if at_edge {
                            self.df_bridge_none[bay] = true;
                        } else if let Some(c) = self.bridge_edit_mut() {
                            c.driver = cycle_slice(&BRIDGE_DRIVERS, c.driver, forward);
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
            F::BridgeSpeed => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.mode = cycle_slice(&BRIDGE_SPEEDS, c.mode, forward);
                }
            }
            F::BridgeServeSpeed => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.speed = cycle_slice(&BRIDGE_SERVE_SPEEDS, c.speed, forward);
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
            | F::ScsiUnit6Boot => {
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
            F::Rtc => self.rtc = !self.rtc,
            F::Identify => self.identify = !self.identify,
            F::Fpu => self.fpu = !self.fpu,
            F::Icache => self.icache = !self.icache,
            F::Dcache => self.dcache = !self.dcache,
            F::Jit => self.jit = !self.jit,
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
            F::FloppySounds => self.floppy_sounds = !self.floppy_sounds,
            F::StartFullscreen => self.start_fullscreen = !self.start_fullscreen,
            F::ShowStatusBar => self.show_status_bar = !self.show_status_bar,
            F::Deinterlace => self.deinterlace = !self.deinterlace,
            F::Bezel => self.bezel = !self.bezel,
            F::PerfOverlay => self.perf_overlay = !self.perf_overlay,
            F::Mt32Panel => self.mt32_panel = !self.mt32_panel,
            F::PowerOn => self.power_on = !self.power_on,
            F::BridgeAutoCache => {
                if let Some(c) = self.bridge_edit_mut() {
                    c.auto_cache = !c.auto_cache;
                }
            }
            F::RealtimePriority => self.realtime_priority = !self.realtime_priority,
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
        let seed_cascade = Self::is_drive_field(field)
            && self.path(field).is_none()
            && !crate::config::is_cd_image_path(&path);
        match field {
            F::Rom => self.rom = Some(path),
            F::Mt32ControlRom => self.mt32_control_rom = Some(path),
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
            F::CdImage => self.cd_image = Some(path),
            F::Cd32Nvram => self.cd32_nvram = Some(path),
            F::ParallelOutput => self.parallel_output = Some(path),
            _ => {
                if let Some((slot, false)) = filesys_slot(field) {
                    self.filesys_dirs[slot] = Some(path);
                }
            }
        }
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
        match field {
            F::Rom => self.rom = None,
            F::ExtendedRom => self.extended_rom = None,
            F::Mt32ControlRom => self.mt32_control_rom = None,
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
            F::CdImage => self.cd_image = None,
            F::Cd32Nvram => self.cd32_nvram = None,
            F::ParallelOutput => self.parallel_output = None,
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
        // A drive's volume name and boot priority are meaningless once its
        // image is gone.
        if Self::is_drive_field(field) {
            self.set_drive_name(field, String::new());
            self.clear_drive_bootpri(field);
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
                }
            }
        }
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
            _ => None,
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
            _ => {}
        }
    }

    /// Whether a boot-priority field is editable: its drive holds a hard-disk
    /// image. Priority is meaningless for an empty slot or a CD image.
    fn boot_field_applies(&self, field: LauncherField) -> bool {
        Self::boot_field_drive(field)
            .and_then(|drive| self.path(drive))
            .is_some_and(|p| !crate::config::is_cd_image_path(p))
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
            _ => scsi_boot_index(field).is_some_and(|i| self.scsi_unit_boot_off[i]),
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
    #[cfg(feature = "floppybridge")]
    pub fn set_drive_bridged(&mut self, idx: usize, on: bool) {
        if idx >= self.df_bridge.len() || self.drive_bridged(idx) == on {
            return;
        }
        if on {
            self.df_playlists[idx].clear();
            self.df_bridge[idx] = Some(FloppyBridgeConfig::default());
            // Look again now: this is the moment the user expects to be told
            // whether there is anything on the other end.
            self.bridge_status = bridge_status();
            self.bridge_ports = sample_bridge_ports();
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

    #[cfg(not(feature = "floppybridge"))]
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

    /// What the library can see, for the FloppyBridge page's heading.
    pub fn bridge_status(&self) -> BridgeStatus {
        self.bridge_status
    }

    /// Which bay the settings page is editing.
    pub fn bridge_edit_drive(&self) -> usize {
        self.bridge_edit_drive
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

    /// The settings being shown on the FloppyBridge page.
    fn bridge_edit(&self) -> Option<&FloppyBridgeConfig> {
        self.df_bridge[self.bridge_edit_drive].as_ref()
    }

    fn bridge_edit_mut(&mut self) -> Option<&mut FloppyBridgeConfig> {
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
    #[cfg(feature = "floppybridge")]
    fn bridge_driver_supports(&self, option: u32) -> bool {
        let Some(cfg) = self.bridge_edit() else {
            return false;
        };
        let token = cfg.driver.match_token();
        // The bridge is linked in, so it always has drivers to describe. If it
        // ever answers with none there is nothing to ask, and leaving every row
        // live is better than greying out a page the user cannot then fix.
        let drivers = crate::floppybridge::drivers();
        if drivers.is_empty() {
            return true;
        }
        drivers
            .iter()
            .find(|d| {
                d.name
                    .to_ascii_lowercase()
                    .replace([' ', '-', '_'], "")
                    .contains(token)
            })
            .is_none_or(|d| d.supports(option))
    }

    /// Serial ports to offer, "Automatic" first -- the default, and what
    /// every current interface supports. The library's own scan leads, then
    /// every other serial device the host has, so an interface on a chip the
    /// scan's naming misses -- an Arduino clone mounting as
    /// `tty.wchusbserial*` on macOS, say -- can still be picked by hand. The
    /// names are the host's own: `/dev/cu.usbmodem101` on macOS,
    /// `/dev/ttyACM0` on Linux, `COM3` on Windows.
    fn bridge_port_options(&self) -> Vec<Option<String>> {
        #[cfg(feature = "floppybridge")]
        {
            self.bridge_ports.clone()
        }
        #[cfg(not(feature = "floppybridge"))]
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

/// A short status/error line shown along the bottom of the configuration panel.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub error: bool,
}

impl StatusMessage {
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
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
}

/// The full interactive state of the open configuration panel.
#[derive(Debug, Clone)]
pub struct LauncherState {
    pub setup: MachineSetup,
    pub tab: LauncherTab,
    pub status: Option<StatusMessage>,
    /// The text field being typed into, and the edit buffer, when one has
    /// focus (a plugin string option or a drive volume name).
    editing: Option<EditTarget>,
    edit_buffer: String,
}

impl LauncherState {
    pub fn new(setup: MachineSetup) -> Self {
        let mut setup = setup;
        // Read the host devices as the screen opens so the pickers show what is
        // connected now.
        setup.refresh_host_devices();
        Self {
            setup,
            tab: LauncherTab::System,
            status: None,
            editing: None,
            edit_buffer: String::new(),
        }
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
        self.status = None;
    }

    /// Focus a drive's volume-name field, seeding the buffer with its value.
    pub fn begin_edit_drive_name(&mut self, field: LauncherField) {
        self.edit_buffer = self.setup.drive_name(field).unwrap_or_default().to_string();
        self.editing = Some(EditTarget::DriveName(field));
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
        self.status = None;
    }

    pub fn edit_push(&mut self, c: char) {
        let Some(target) = self.editing else { return };
        // A boot priority is a signed integer: digits, and a leading minus.
        if let EditTarget::DriveBootpri(_) = target {
            let minus_ok = c == '-' && self.edit_buffer.is_empty();
            if !(c.is_ascii_digit() || minus_ok) {
                return;
            }
        }
        self.edit_buffer.push(c);
    }

    pub fn edit_backspace(&mut self) {
        if self.editing.is_some() {
            self.edit_buffer.pop();
        }
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
            EditTarget::BoardOption { .. } => {}
        }
        self.editing = None;
        let value = std::mem::take(&mut self.edit_buffer);
        match target {
            EditTarget::BoardOption { board, opt } => {
                self.setup.zorro_option_set(board, opt, value)
            }
            EditTarget::DriveName(field) => self.setup.set_drive_name(field, value),
            EditTarget::DriveBootpri(_) => {}
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
    #[cfg(all(feature = "midi", feature = "mt32"))]
    let serial: &[&[Row]] = &[&SERIAL_ROWS_MIDI, &SERIAL_ROWS_MT32];
    #[cfg(all(feature = "midi", not(feature = "mt32")))]
    let serial: &[&[Row]] = &[&SERIAL_ROWS_MIDI];
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
        &CD_ROWS,
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
fn drive_raw(path: Option<&Path>, name: Option<&str>, bootpri: Option<i8>) -> Option<RawDrive> {
    path.map(|p| RawDrive {
        path: path_string(p),
        name: name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        bootpri,
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
    }
}

fn rtg_card_value(card: RtgCard) -> &'static str {
    match card {
        RtgCard::None => "none",
        RtgCard::Picasso2 => "picasso2",
        RtgCard::Picasso2Plus => "picasso2plus",
        RtgCard::Z3660 => "z3660",
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

    /// The interfaces genuinely differ in what they honour, and the page says
    /// so: a DrawBridge has no drive-select line on its cable, a Greaseweazle
    /// does. Only meaningful with the library installed -- without it there is
    /// nothing to ask, and every row deliberately stays live.
    #[cfg(feature = "floppybridge")]
    #[test]
    fn bridge_rows_grey_what_the_interface_does_not_support() {
        if crate::floppybridge::drivers().is_empty() {
            return;
        }
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

        for (driver, cable_greyed) in [
            (crate::config::BridgeDriver::Greaseweazle, false),
            (crate::config::BridgeDriver::DrawBridge, true),
        ] {
            setup.df_bridge[0].as_mut().expect("bridged").driver = driver;
            assert_eq!(
                setup.disabled_reason(F::BridgeCable).is_some(),
                cable_greyed,
                "drive select on {driver:?}"
            );
            // Every interface here talks over a serial port and can auto-cache.
            assert!(setup.disabled_reason(F::BridgePort).is_none());
            assert!(setup.disabled_reason(F::BridgeAutoCache).is_none());
        }
    }

    /// Drive speed acts on image bays only, so the row greys exactly when
    /// every fitted bay is physical: one image bay anywhere keeps it live.
    #[cfg(feature = "floppybridge")]
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
    #[cfg(feature = "floppybridge")]
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
            F::BridgeSpeed,
            F::BridgeServeSpeed,
            F::BridgeAutoCache,
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
        for f in [F::BridgeDensity, F::BridgeSpeed, F::BridgeServeSpeed] {
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
    #[cfg(feature = "floppybridge")]
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
    #[cfg(feature = "floppybridge")]
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
    #[cfg(feature = "floppybridge")]
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
    #[cfg(feature = "floppybridge")]
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
    #[cfg(feature = "floppybridge")]
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
        assert!(status.error);
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
        s.toggle(LauncherField::Mt32Panel);
        assert!(s.toggle_value(LauncherField::Mt32Panel));

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
    fn bezel_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // Off is the baseline, so nothing is written for it.
        assert!(!s.toggle_value(LauncherField::Bezel));
        assert_eq!(s.to_raw().display.bezel, None);

        s.toggle(LauncherField::Bezel);
        assert!(s.toggle_value(LauncherField::Bezel));
        assert_eq!(s.to_raw().display.bezel, Some(true));

        // The written config has to load back into the same setting.
        assert!(s.build_config().expect("valid config").bezel);
        let reloaded = MachineSetup::from_raw(&s.to_raw()).expect("valid raw");
        assert!(reloaded.toggle_value(LauncherField::Bezel));
    }

    #[test]
    fn deinterlace_round_trips_through_raw() {
        let mut s = MachineSetup::default();
        // On is the baseline, so nothing is written for it.
        assert!(s.toggle_value(LauncherField::Deinterlace));
        assert_eq!(s.to_raw().display.deinterlace, None);

        s.toggle(LauncherField::Deinterlace);
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
        // A launcher save must not drop the [serial] listen override, which
        // no tab edits (regression: it was absent from MachineSetup/to_raw).
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
    fn serial_mode_cycle_offers_tcp_connect_only_with_an_address() {
        // The launcher has no editor for the dial-out address, so without
        // one in the loaded config the mode would always fail at Run;
        // cycling skips it (same pattern as the printer capture path).
        let mut setup = MachineSetup {
            serial_mode: SerialMode::Tcp,
            ..Default::default()
        };
        setup.cycle(LauncherField::SerialMode, true);
        assert_eq!(setup.serial_mode, SerialMode::Pty);

        let mut setup = MachineSetup {
            serial_mode: SerialMode::Tcp,
            serial_connect: Some("bbs.example.com:1337".into()),
            ..Default::default()
        };
        setup.cycle(LauncherField::SerialMode, true);
        assert_eq!(setup.serial_mode, SerialMode::TcpConnect);
    }

    #[test]
    fn serial_tcp_connect_round_trips_through_raw() {
        // Same contract as the listen override: the dial-out address has no
        // launcher editor, so loading and saving must carry it unchanged.
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

    #[test]
    fn sub_pages_of_hdd_cd() {
        // The sub-pages (CD included) are not top-level strip tabs.
        for t in [
            LauncherTab::Cd,
            LauncherTab::HostFs,
            LauncherTab::BootPriority,
        ] {
            assert!(!TABS.contains(&t));
            // Each keeps the Storage strip tab highlighted and returns to it.
            assert_eq!(t.strip_tab(), LauncherTab::Storage);
            assert_eq!(t.parent_tab(), Some(LauncherTab::Storage));
        }
        // A top-level tab has no parent.
        assert_eq!(LauncherTab::Storage.parent_tab(), None);

        // The Storage nav lists CD, Host Mounts, Boot Priority in that order
        // (drawn as a fixed top nav row, not a settings row).
        let storage_nav: Vec<_> = LauncherTab::Storage
            .nav_options()
            .iter()
            .map(|&(_, t)| t)
            .collect();
        assert_eq!(
            storage_nav,
            [
                LauncherTab::Cd,
                LauncherTab::HostFs,
                LauncherTab::BootPriority
            ]
        );

        // The Storage tab is just the storage rows (IDE/SCSI options).
        let storage = rows(
            LauncherTab::Storage,
            ParallelDevice::None,
            SerialMode::default(),
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
        ] {
            let page = rows(tab, ParallelDevice::None, SerialMode::default(), false);
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
        for t in [LauncherTab::AvVideo, LauncherTab::AvEmulation] {
            // They keep the A/V strip entry lit and have no Back button --
            // categories switch between each other via the top nav row.
            assert_eq!(t.strip_tab(), LauncherTab::AvAudio);
            assert_eq!(t.parent_tab(), None);
        }
        // Every A/V page offers the same nav buttons: Audio / Video / Emulation.
        let nav = LauncherTab::AvAudio.nav_options();
        assert_eq!(
            nav,
            [
                ("Audio", LauncherTab::AvAudio),
                ("Video", LauncherTab::AvVideo),
                ("Emulation", LauncherTab::AvEmulation),
            ]
        );
        assert_eq!(LauncherTab::AvVideo.nav_options(), nav);
        assert!(LauncherTab::AvAudio.has_top_nav());
        assert!(LauncherTab::Storage.has_top_nav());
        assert!(!LauncherTab::System.has_top_nav());

        // Each category shows only its own settings; the default is Audio.
        let page = |t| rows(t, ParallelDevice::None, SerialMode::default(), false);
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

    #[cfg(feature = "midi")]
    #[test]
    fn io_ports_tab_groups_serial_parallel_and_ethernet_under_headers() {
        let r = rows(
            LauncherTab::IoPorts,
            ParallelDevice::None,
            SerialMode::Midi,
            false,
        );
        let headers: Vec<_> = r
            .iter()
            .filter(|x| x.kind == RowKind::SectionHeader)
            .map(|x| x.label)
            .collect();
        assert_eq!(headers, ["Serial:", "Parallel:", "Ethernet:"]);
        // Serial section: the Device / Mode selector, and (in MIDI) the endpoints.
        assert!(r.iter().any(|x| x.field == LauncherField::SerialMode));
        assert!(r.iter().any(|x| x.field == LauncherField::MidiOut));
        // Parallel section: the device selector.
        assert!(r.iter().any(|x| x.field == LauncherField::ParallelDevice));
        // Ethernet section: the A2065 board selector.
        assert!(r.iter().any(|x| x.field == LauncherField::Ethernet));
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
            rows(LauncherTab::IoPorts, device, SerialMode::default(), false)
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
            rows(LauncherTab::IoPorts, ParallelDevice::None, mode, false)
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
            rows(LauncherTab::IoPorts, device, SerialMode::default(), false)
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
        s.toggle(LauncherField::StartFullscreen);
        s.toggle(LauncherField::ShowStatusBar);
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
