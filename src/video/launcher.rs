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
    format_size, machine_profile_defaults, ChannelMode, Chipset, Config, CpuModel,
    JoystickInputMode, MachineModel, Overscan, PacingBudget, ParallelDevice, PixelAspect,
    RawConfig, RawDrive, RawFilesysMount, RawFloppyDrive, RawZorroBoard, RtgCard, ScsiController,
    SerialMode, WarpSpeed,
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
    HostFs,
    Cd,
    /// Serial and parallel ports on one tab, under `Serial:` / `Parallel:`
    /// section headings.
    IoPorts,
    Input,
    Zorro,
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
    // HostFs is reached as a sub-page from the Hard Disk (Storage) tab, so it is
    // not a top-level strip entry.
    LauncherTab::Cd,
    LauncherTab::Input,
    LauncherTab::IoPorts,
    LauncherTab::Zorro,
    LauncherTab::AvEmulation,
];

impl LauncherTab {
    pub fn label(self) -> &'static str {
        match self {
            LauncherTab::System => "System",
            LauncherTab::Cpu => "CPU",
            LauncherTab::Memory => "Memory",
            LauncherTab::Rom => "ROM",
            LauncherTab::Floppy => "Floppy",
            LauncherTab::Storage => "Hard Disk",
            LauncherTab::HostFs => "Host Mounts",
            LauncherTab::Cd => "CD",
            LauncherTab::IoPorts => "I/O Ports",
            LauncherTab::Input => "Input",
            LauncherTab::Zorro => "Zorro",
            LauncherTab::AvEmulation => "A/V & Emu",
        }
    }

    /// The strip entry to highlight for this (possibly sub-page) tab: the Host
    /// Mounts sub-page keeps its parent Hard Disk tab highlighted.
    pub fn strip_tab(self) -> LauncherTab {
        match self {
            LauncherTab::HostFs => LauncherTab::Storage,
            other => other,
        }
    }
}

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
    /// Inert field for a non-interactive [`RowKind::SectionHeader`] row.
    SectionHeader,
    // A/V and emulation
    AudioDevice,
    AudioChannelMode,
    AudioStereoSeparation,
    Overscan,
    PixelAspect,
    Phosphor,
    FloppySounds,
    FloppyVolume,
    PowerOn,
    PacingBudget,
    RealtimePriority,
    Warp,
    // Input
    Joystick,
    Port1Device,
    Port2Device,
}

/// How a row's value is edited, and therefore which widget the panel draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A `[<] value [>]` picker / stepper.
    Cycle,
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
    /// A button that navigates to another tab used as a sub-page (the Hard Disk
    /// tab's link to Host Mounts, and the Back link the other way). Its `field`
    /// is inert; the payload is the destination tab.
    SubPageLink(LauncherTab),
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

/// A row whose button navigates to `target` used as a sub-page (see
/// [`RowKind::SubPageLink`]).
const fn sub_page_link(label: &'static str, target: LauncherTab) -> Row {
    Row {
        field: F::SectionHeader,
        label,
        kind: RowKind::SubPageLink(target),
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
use RowKind::{Cycle, Drive, Toggle};
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
const CPU_ROWS: [Row; 5] = [
    row(F::Cpu, "CPU", Cycle),
    row(F::Fpu, "FPU (68881/2)", Toggle),
    row(F::Clock, "Clock", Cycle),
    row(F::Icache, "Instruction cache", Toggle),
    row(F::Dcache, "Data cache", Toggle),
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
const FLOPPY_ROWS: [Row; 10] = [
    row(F::FloppyDrives, "Drives", Cycle),
    row(F::FloppySpeed, "Drive speed", Cycle),
    row(F::Df0Image, "DF0 image", PathRow),
    row(F::Df0WriteProtect, "DF0 write-protect", Toggle),
    row(F::Df1Image, "DF1 image", PathRow),
    row(F::Df1WriteProtect, "DF1 write-protect", Toggle),
    row(F::Df2Image, "DF2 image", PathRow),
    row(F::Df2WriteProtect, "DF2 write-protect", Toggle),
    row(F::Df3Image, "DF3 image", PathRow),
    row(F::Df3WriteProtect, "DF3 write-protect", Toggle),
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
const CD_ROWS: [Row; 3] = [
    row(F::CdImage, "CD image", PathRow),
    row(F::CdInsertDelay, "Insert delay", Cycle),
    row(F::Cd32Nvram, "CD32 NVRAM", PathRow),
];
// The MIDI endpoint rows appear only when the serial port is in MIDI mode, so
// the Serial section shows just the Device / Mode selector otherwise. The
// selector is labelled "Device / Mode" because some choices are devices (MIDI)
// and some are modes (stdout, PTY, TCP).
#[cfg(feature = "midi")]
const SERIAL_ROWS_BASE: [Row; 1] = [row(F::SerialMode, "Device / Mode", Cycle)];
#[cfg(feature = "midi")]
const SERIAL_ROWS_MIDI: [Row; 3] = [
    row(F::SerialMode, "Device / Mode", Cycle),
    row(F::MidiIn, "MIDI input", Cycle),
    row(F::MidiOut, "MIDI output", Cycle),
];
// The sampler input/gain rows appear only when the sampler is the selected
// device, so None/Printer show just the Device selector.
const PARALLEL_ROWS_BASE: [Row; 1] = [row(F::ParallelDevice, "Device", Cycle)];
// The printer adds a capture-file picker; the sampler adds its input/gain rows.
const PARALLEL_ROWS_PRINTER: [Row; 2] = [
    row(F::ParallelDevice, "Device", Cycle),
    row(F::ParallelOutput, "Output file", PathRow),
];
const PARALLEL_ROWS_SAMPLER: [Row; 3] = [
    row(F::ParallelDevice, "Device", Cycle),
    row(F::SamplerInput, "Audio input", Cycle),
    row(F::SamplerGain, "Input gain", Cycle),
];
const ETHERNET_ROWS: [Row; 1] = [row(F::Ethernet, "A2065 board", Cycle)];
const AV_EMULATION_ROWS: [Row; 12] = [
    row(F::AudioDevice, "Audio output", Cycle),
    row(F::AudioChannelMode, "Channel mode", Cycle),
    row(F::AudioStereoSeparation, "Stereo separation", Cycle),
    row(F::Overscan, "Overscan", Cycle),
    row(F::PixelAspect, "Pixel aspect", Cycle),
    row(F::Phosphor, "Phosphor", Cycle),
    row(F::FloppySounds, "Floppy sounds", Toggle),
    row(F::FloppyVolume, "Floppy volume", Cycle),
    row(F::PowerOn, "Power on at start", Toggle),
    row(F::PacingBudget, "Pacing budget", Cycle),
    row(F::RealtimePriority, "Realtime priority", Toggle),
    row(F::Warp, "Warp speed", Cycle),
];
const INPUT_ROWS: [Row; 3] = [
    row(F::Port1Device, "Port 1", Cycle),
    row(F::Port2Device, "Port 2", Cycle),
    row(F::Joystick, "Joystick input", Cycle),
];

/// The rows shown on a tab, top to bottom. Most tabs are fixed and borrow their
/// static row table; only the three composed tabs (Storage/HostFs with their
/// sub-page links, and the dynamic I/O Ports tab) allocate. The I/O Ports tab is
/// dynamic: the MIDI endpoint rows appear only in MIDI mode and the
/// sampler/printer rows only for those devices, so unrelated options stay hidden
/// rather than greyed. The `Zorro` tab has no rows: it is drawn as a board list
/// with Add/Remove controls (see the panel code).
pub fn rows(
    tab: LauncherTab,
    parallel_device: ParallelDevice,
    serial_mode: SerialMode,
) -> Cow<'static, [Row]> {
    match tab {
        LauncherTab::System => Cow::Borrowed(&SYSTEM_ROWS),
        LauncherTab::Cpu => Cow::Borrowed(&CPU_ROWS),
        LauncherTab::Memory => Cow::Borrowed(&MEMORY_ROWS),
        LauncherTab::Rom => Cow::Borrowed(&ROM_ROWS),
        LauncherTab::Floppy => Cow::Borrowed(&FLOPPY_ROWS),
        LauncherTab::Storage => {
            // The Hard Disk tab links to the Host Mounts sub-page, at the top so
            // it sits where the sub-page's own Back link is.
            let mut rows = vec![sub_page_link("Host Mounts", LauncherTab::HostFs)];
            rows.extend_from_slice(&STORAGE_ROWS);
            Cow::Owned(rows)
        }
        LauncherTab::HostFs => {
            // The Host Mounts sub-page opens with a link back to Hard Disk.
            let mut rows = vec![sub_page_link("< Hard Disk", LauncherTab::Storage)];
            rows.extend_from_slice(&HOSTFS_ROWS);
            Cow::Owned(rows)
        }
        LauncherTab::Cd => Cow::Borrowed(&CD_ROWS),
        LauncherTab::IoPorts => Cow::Owned(io_ports_rows(serial_mode, parallel_device)),
        LauncherTab::Input => Cow::Borrowed(&INPUT_ROWS),
        LauncherTab::Zorro => Cow::Borrowed(&[]),
        LauncherTab::AvEmulation => Cow::Borrowed(&AV_EMULATION_ROWS),
    }
}

/// The I/O Ports tab: a `Serial:` section (only in a `midi` build, which is the
/// only build with serial rows), a `Parallel:` section, then an `Ethernet:`
/// section, each under a greyed heading and each showing only the rows relevant
/// to its selected device/mode.
fn io_ports_rows(serial_mode: SerialMode, parallel_device: ParallelDevice) -> Vec<Row> {
    let mut rows = Vec::new();
    let serial = serial_rows(serial_mode);
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
fn serial_rows(serial_mode: SerialMode) -> &'static [Row] {
    #[cfg(feature = "midi")]
    {
        if serial_mode == SerialMode::Midi {
            &SERIAL_ROWS_MIDI
        } else {
            &SERIAL_ROWS_BASE
        }
    }
    #[cfg(not(feature = "midi"))]
    {
        let _ = serial_mode;
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
const RTG_CARDS: [RtgCard; 2] = [RtgCard::None, RtgCard::Z3660];
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
const CLOCK_PRESETS: [f64; 8] = [7.09, 14.0, 14.18, 25.0, 28.0, 33.0, 40.0, 50.0];
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
const FLOPPY_SPEEDS: [u16; 5] = [100, 200, 400, 800, crate::floppy::SPEED_TURBO];
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
// `None` = no A2065 fitted; `Some(NetConfig::None)` fits the board with an
// isolated NIC (the guest sees the hardware but no traffic ever arrives).
const ETHERNET_CHOICES: [Option<NetConfig>; 4] = [
    None,
    Some(NetConfig::None),
    Some(NetConfig::Loopback),
    Some(NetConfig::Nat),
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
    // CPU
    cpu: CpuModel,
    fpu: bool,
    clock_mhz: f64,
    icache: bool,
    dcache: bool,
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
    // Hard disk. Each drive's optional volume-name override (directory mounts
    // only) sits in the matching `*_name` slot, paralleling the path slot.
    ide_master: Option<PathBuf>,
    ide_master_name: Option<String>,
    ide_slave: Option<PathBuf>,
    ide_slave_name: Option<String>,
    /// Which SCSI host adapter is fitted, or `None` for no board. Shares the
    /// `scsi_*` ROM/unit block below (the drives are portable between boards).
    scsi_controller: Option<ScsiController>,
    scsi_rom: Option<PathBuf>,
    scsi_rom_odd: Option<PathBuf>,
    scsi_units: [Option<PathBuf>; 7],
    scsi_unit_names: [Option<String>; 7],
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
    overscan: Overscan,
    pixel_aspect: PixelAspect,
    phosphor: f32,
    floppy_sounds: bool,
    floppy_volume: u8,
    power_on: bool,
    pacing_budget: PacingBudget,
    realtime_priority: bool,
    warp: WarpSpeed,
    joystick_input_mode: JoystickInputMode,
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
        let df_write_protected = std::array::from_fn(|i| {
            cfg.floppy.drives[i]
                .as_ref()
                .map(|d| d.write_protected)
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
            cpu: cfg.cpu,
            fpu: cfg.fpu,
            clock_mhz: cfg.cpu_clock_mhz,
            icache: cfg.cpu_icache,
            dcache: cfg.cpu_dcache,
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
            ide_master: cfg.ide.master.as_ref().map(|d| d.path.clone()),
            ide_master_name: cfg.ide.master.as_ref().and_then(|d| d.volume_name.clone()),
            ide_slave: cfg.ide.slave.as_ref().map(|d| d.path.clone()),
            ide_slave_name: cfg.ide.slave.as_ref().and_then(|d| d.volume_name.clone()),
            scsi_controller: cfg.scsi.enabled().then_some(cfg.scsi.controller),
            scsi_rom: cfg.scsi.rom.clone(),
            scsi_rom_odd: cfg.scsi.rom_odd.clone(),
            scsi_units: std::array::from_fn(|i| cfg.scsi.units[i].as_ref().map(|d| d.path.clone())),
            scsi_unit_names: std::array::from_fn(|i| {
                cfg.scsi.units[i]
                    .as_ref()
                    .and_then(|d| d.volume_name.clone())
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
            a2065_net: cfg.a2065_net,
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
            overscan: cfg.overscan,
            pixel_aspect: cfg.pixel_aspect,
            phosphor: cfg.phosphor,
            floppy_sounds: cfg.audio.floppy_sounds,
            floppy_volume: cfg.audio.floppy_sounds_volume,
            power_on: cfg.emulation.power_on,
            pacing_budget: cfg.emulation.pacing_budget,
            realtime_priority: cfg.emulation.realtime_priority,
            warp: cfg.emulation.warp_speed,
            joystick_input_mode: cfg.joystick_input_mode,
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
    /// sees traffic, so only NAT qualifies.
    pub fn ethernet_breaks_determinism(&self) -> bool {
        self.a2065_net == Some(NetConfig::Nat)
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
            raw.rtg.card = Some(rtg_card_name(self.rtg).to_ascii_lowercase());
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
        raw.ide.master = drive_raw(self.ide_master.as_deref(), self.ide_master_name.as_deref());
        raw.ide.slave = drive_raw(self.ide_slave.as_deref(), self.ide_slave_name.as_deref());
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
            );
            raw.scsi.unit1 = drive_raw(
                self.scsi_units[1].as_deref(),
                self.scsi_unit_names[1].as_deref(),
            );
            raw.scsi.unit2 = drive_raw(
                self.scsi_units[2].as_deref(),
                self.scsi_unit_names[2].as_deref(),
            );
            raw.scsi.unit3 = drive_raw(
                self.scsi_units[3].as_deref(),
                self.scsi_unit_names[3].as_deref(),
            );
            raw.scsi.unit4 = drive_raw(
                self.scsi_units[4].as_deref(),
                self.scsi_unit_names[4].as_deref(),
            );
            raw.scsi.unit5 = drive_raw(
                self.scsi_units[5].as_deref(),
                self.scsi_unit_names[5].as_deref(),
            );
            raw.scsi.unit6 = drive_raw(
                self.scsi_units[6].as_deref(),
                self.scsi_unit_names[6].as_deref(),
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
        if (self.phosphor - base.phosphor).abs() > 1e-6 {
            raw.display.phosphor = Some(self.phosphor);
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
            .map(|n| crate::net::net_config_name(n).to_string());
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
        self.cpu = base.cpu;
        self.fpu = base.fpu;
        self.clock_mhz = base.cpu_clock_mhz;
        self.icache = base.cpu_icache;
        self.dcache = base.cpu_dcache;
        self.chip_ram = base.chip_ram_bytes;
        self.fast_ram = base.fast_ram_bytes;
        self.slow_ram = base.slow_ram_bytes;
        self.mb_ram = base.mb_ram_bytes;
        self.accel_ram = base.accel_ram_bytes;
        self.z3_ram = base.z3_ram_bytes;
        self.overscan = base.overscan;
        self.pixel_aspect = base.pixel_aspect;
        self.phosphor = base.phosphor;
        self.floppy_sounds = base.audio.floppy_sounds;
        self.floppy_volume = base.audio.floppy_sounds_volume;
        self.power_on = base.emulation.power_on;
        self.pacing_budget = base.emulation.pacing_budget;
        self.realtime_priority = base.emulation.realtime_priority;
        self.warp = base.emulation.warp_speed;
        self.joystick_input_mode = base.joystick_input_mode;
        self.port_devices = base.port_devices;
        if !self.has_ide() {
            self.ide_master = None;
            self.ide_master_name = None;
            self.ide_slave = None;
            self.ide_slave_name = None;
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
            F::Z3Ram => reason(cpu_is_32bit(self.cpu), "needs 32-bit CPU"),
            // The CPU-slot space at $08000000 is beyond a 24-bit bus too.
            F::AccelRam => reason(cpu_is_32bit(self.cpu), "needs 32-bit CPU"),
            // The Z3660 is a Zorro III board: same address-reach gate.
            F::Rtg => reason(cpu_is_32bit(self.cpu), "needs 32-bit CPU"),
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
            // A boot priority or read-only flag is meaningless without a
            // directory to mount.
            F::Filesys0Boot | F::Filesys1Boot | F::Filesys2Boot | F::Filesys3Boot => {
                let (slot, _) = filesys_slot(field).expect("boot field");
                reason(self.filesys_dirs[slot].is_some(), "no directory")
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
            F::AudioChannelMode => reason(self.audio_output.is_enabled(), "off"),
            F::AudioStereoSeparation => {
                if !self.audio_output.is_enabled() {
                    Some("off")
                } else {
                    reason(self.audio_channel_mode != ChannelMode::Mono, "mono")
                }
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
            F::Df0WriteProtect => self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3],
            F::FloppySounds => self.floppy_sounds,
            F::PowerOn => self.power_on,
            F::RealtimePriority => self.realtime_priority,
            _ => false,
        }
    }

    /// The current path of a path field, if any.
    pub fn path(&self, field: LauncherField) -> Option<&Path> {
        match field {
            F::Rom => self.rom.as_deref(),
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
            F::Phosphor => {
                if self.phosphor <= 0.0 {
                    "Off".to_string()
                } else {
                    format!("{:.2}", self.phosphor)
                }
            }
            F::FloppyVolume => format!("{}%", self.floppy_volume),
            F::PacingBudget => match self.pacing_budget {
                PacingBudget::Cycles => "Cycles".to_string(),
                PacingBudget::Instructions => "Instructions".to_string(),
            },
            F::Warp => self.warp.label().to_string(),
            F::Joystick => match self.joystick_input_mode {
                JoystickInputMode::Keyboard => "Keyboard".to_string(),
                JoystickInputMode::Gamepad => "Gamepad".to_string(),
            },
            F::Port1Device => port_device_display(self.port_devices[0]).to_string(),
            F::Port2Device => port_device_display(self.port_devices[1]).to_string(),
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
            F::MidiOut => self.midi_out.clone().unwrap_or_else(|| "None".to_string()),
            #[cfg(feature = "midi")]
            F::MidiIn => self.midi_in.clone().unwrap_or_else(|| "None".to_string()),
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
            F::Ethernet => match self.a2065_net {
                None => "Not fitted".to_string(),
                Some(NetConfig::None) => "Isolated".to_string(),
                Some(NetConfig::Loopback) => "Loopback".to_string(),
                Some(NetConfig::Nat) => "NAT".to_string(),
            },
            F::AudioDevice => self.audio_output.label().to_string(),
            F::AudioChannelMode => match self.audio_channel_mode {
                ChannelMode::Stereo => "Stereo".to_string(),
                ChannelMode::Mono => "Mono".to_string(),
            },
            F::AudioStereoSeparation => format!("{}%", self.audio_stereo_separation),
            F::Filesys0Boot | F::Filesys1Boot | F::Filesys2Boot | F::Filesys3Boot => {
                let (slot, _) = filesys_slot(field).expect("boot field");
                match self.filesys_bootpri[slot] {
                    -128 => "Never".to_string(),
                    pri => pri.to_string(),
                }
            }
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
                || rows_contains_kind(field, RowKind::Drive) =>
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

    /// Step a cycle/stepper field forward (`forward`) or backward.
    pub fn cycle(&mut self, field: LauncherField, forward: bool) {
        match field {
            F::Chipset => self.chipset = cycle_slice(&CHIPSETS, self.chipset, forward),
            F::Rtg => self.rtg = cycle_slice(&RTG_CARDS, self.rtg, forward),
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
                    // keeps the emitted config launchable.
                    self.z3_ram = 0;
                    self.mb_ram = 0;
                    self.accel_ram = 0;
                    self.rtg = RtgCard::None;
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
            F::FloppyVolume => self.floppy_volume = step_u8(self.floppy_volume, forward, 0, 100),
            F::Overscan => self.overscan = cycle_slice(&OVERSCANS, self.overscan, forward),
            F::PixelAspect => {
                self.pixel_aspect = cycle_slice(&PIXEL_ASPECTS, self.pixel_aspect, forward)
            }
            F::PacingBudget => {
                self.pacing_budget = cycle_slice(&PACINGS, self.pacing_budget, forward)
            }
            F::Warp => self.warp = cycle_slice(&WARPS, self.warp, forward),
            F::Joystick => {
                self.joystick_input_mode =
                    cycle_slice(&JOYSTICK_MODES, self.joystick_input_mode, forward)
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
            F::MidiOut => cycle_endpoint(&mut self.midi_out, &self.midi_endpoints.outputs, forward),
            #[cfg(feature = "midi")]
            F::MidiIn => cycle_endpoint(&mut self.midi_in, &self.midi_endpoints.inputs, forward),
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
                // NAT is only on offer when the userspace NAT is compiled in;
                // without it the choice would fit a board that never connects.
                let choices: Vec<Option<NetConfig>> = ETHERNET_CHOICES
                    .into_iter()
                    .filter(|c| cfg!(feature = "net-nat") || *c != Some(NetConfig::Nat))
                    .collect();
                self.a2065_net = cycle_slice(&choices, self.a2065_net, forward);
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
            F::AudioStereoSeparation => {
                self.audio_stereo_separation = cycle_nearest(
                    &STEREO_SEPARATION_STEPS,
                    usize::from(self.audio_stereo_separation),
                    forward,
                ) as u8
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
            F::Df0WriteProtect => self.df_write_protected[0] = !self.df_write_protected[0],
            F::Df1WriteProtect => self.df_write_protected[1] = !self.df_write_protected[1],
            F::Df2WriteProtect => self.df_write_protected[2] = !self.df_write_protected[2],
            F::Df3WriteProtect => self.df_write_protected[3] = !self.df_write_protected[3],
            F::FloppySounds => self.floppy_sounds = !self.floppy_sounds,
            F::PowerOn => self.power_on = !self.power_on,
            F::RealtimePriority => self.realtime_priority = !self.realtime_priority,
            _ => {}
        }
    }

    /// Set a path field's value (a floppy image replaces that drive's
    /// playlist with a single disk and wires the drive in).
    pub fn set_path(&mut self, field: LauncherField, path: PathBuf) {
        match field {
            F::Rom => self.rom = Some(path),
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
        // A drive's volume name is meaningless once its image is gone.
        if Self::is_drive_field(field) {
            self.set_drive_name(field, String::new());
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

    pub fn edit_push(&mut self, c: char) {
        if self.editing.is_some() {
            self.edit_buffer.push(c);
        }
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
        if let EditTarget::DriveName(_) = target {
            let name = self.edit_buffer.trim();
            let invalid = (!name.is_empty())
                .then(|| crate::filesys::volume_name_error(name))
                .flatten();
            if let Some(err) = invalid {
                self.status = Some(StatusMessage::err(err));
                return;
            }
        }
        self.editing = None;
        let value = std::mem::take(&mut self.edit_buffer);
        match target {
            EditTarget::BoardOption { board, opt } => {
                self.setup.zorro_option_set(board, opt, value)
            }
            EditTarget::DriveName(field) => self.setup.set_drive_name(field, value),
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

/// Step a MIDI endpoint selection through "None" then the available endpoints,
/// storing the chosen device's exact name.
#[cfg(feature = "midi")]
fn cycle_endpoint(
    current: &mut Option<String>,
    endpoints: &[crate::midi::MidiEndpoint],
    forward: bool,
) {
    let names: Vec<String> = endpoints.iter().map(|e| e.name.clone()).collect();
    *current = crate::midi::next_endpoint(current.as_deref(), &names, forward);
}

/// Whether `field` appears anywhere with the given row kind. Used to classify a
/// field (toggle vs path) without threading the tab through every call, called
/// per drawn row, so it scans the static row tables directly rather than
/// composing tabs (which would allocate every frame). The composed tabs only
/// add `SectionHeader`/`SubPageLink` rows, which carry no real field, so the raw
/// tables cover every classifiable field.
fn rows_contains_kind(field: LauncherField, kind: RowKind) -> bool {
    #[cfg(feature = "midi")]
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
        &AV_EMULATION_ROWS,
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
fn drive_raw(path: Option<&Path>, name: Option<&str>) -> Option<RawDrive> {
    path.map(|p| RawDrive {
        path: path_string(p),
        name: name
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// Boot priorities offered on the Host FS boot-pri stepper. -128 is the
/// "never boot" sentinel; the rest bracket the usual device priorities
/// (hard-disk partitions boot at 0, DF0: at 5).
const BOOTPRI_STEPS: [i8; 8] = [-128, -10, -5, 0, 5, 6, 10, 20];

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

/// Human form of a port device for the picker rows (the config strings
/// stay lowercase).
fn port_device_display(device: PortDevice) -> &'static str {
    match device {
        PortDevice::Mouse => "Mouse",
        PortDevice::Joystick => "Joystick",
        PortDevice::Cd32Pad => "CD32 pad",
        PortDevice::Analogue => "Analogue",
        PortDevice::None => "None",
    }
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
        RtgCard::Z3660 => "Z3660",
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

fn pacing_name(pacing: PacingBudget) -> &'static str {
    match pacing {
        PacingBudget::Cycles => "cycles",
        PacingBudget::Instructions => "instructions",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // A 68000 machine cannot host the board, so it has none and the row
        // still cycles without producing an unbuildable config.
        s.select_model(Some(MachineModel::A500));
        assert_eq!(s.rtg, RtgCard::None);
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
    fn host_mounts_is_a_sub_page_of_hard_disk() {
        // Host Mounts is not a top-level strip tab any more.
        assert!(!TABS.contains(&LauncherTab::HostFs));
        // The Hard Disk tab opens with a link into the Host Mounts sub-page.
        let storage = rows(
            LauncherTab::Storage,
            ParallelDevice::None,
            SerialMode::default(),
        );
        assert_eq!(
            storage.first().map(|r| r.kind),
            Some(RowKind::SubPageLink(LauncherTab::HostFs))
        );
        // The sub-page opens with a Back link, then the mount rows.
        let mounts = rows(
            LauncherTab::HostFs,
            ParallelDevice::None,
            SerialMode::default(),
        );
        assert_eq!(
            mounts.first().map(|r| r.kind),
            Some(RowKind::SubPageLink(LauncherTab::Storage))
        );
        assert!(mounts.iter().any(|r| r.field == LauncherField::Filesys0Dir));
        // The sub-page keeps the Hard Disk strip tab highlighted.
        assert_eq!(LauncherTab::HostFs.strip_tab(), LauncherTab::Storage);
    }

    #[cfg(feature = "midi")]
    #[test]
    fn io_ports_tab_groups_serial_parallel_and_ethernet_under_headers() {
        let r = rows(LauncherTab::IoPorts, ParallelDevice::None, SerialMode::Midi);
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
        assert_eq!(s.value_label(LauncherField::Ethernet), "Not fitted");
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

        #[cfg(feature = "net-nat")]
        {
            s.cycle(LauncherField::Ethernet, true);
            assert_eq!(s.value_label(LauncherField::Ethernet), "NAT");
            // Only NAT carries traffic on the host's schedule.
            assert!(s.ethernet_breaks_determinism());
            assert_eq!(s.to_raw().a2065.net.as_deref(), Some("nat"));
            s.cycle(LauncherField::Ethernet, true);
        }
        #[cfg(not(feature = "net-nat"))]
        {
            // Without the userspace NAT the picker skips straight past it.
            s.cycle(LauncherField::Ethernet, true);
        }
        assert_eq!(s.value_label(LauncherField::Ethernet), "Not fitted");
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
            rows(LauncherTab::IoPorts, device, SerialMode::default())
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
            rows(LauncherTab::IoPorts, ParallelDevice::None, mode)
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
            rows(LauncherTab::IoPorts, device, SerialMode::default())
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
        // The profile's Zorro III RTG card is beyond a 24-bit bus too.
        assert_eq!(s.rtg, RtgCard::None);
        assert_eq!(
            s.disabled_reason(LauncherField::Rtg),
            Some("needs 32-bit CPU")
        );
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
    fn disabled_audio_greys_out_channel_mode_and_separation() {
        use crate::audio::AudioOutput;
        let mut s = MachineSetup::default();
        // Enabled: channel mode is active, separation active in stereo.
        assert_eq!(s.disabled_reason(LauncherField::AudioChannelMode), None);
        assert_eq!(
            s.disabled_reason(LauncherField::AudioStereoSeparation),
            None
        );

        // Disabled audio greys both shaping controls.
        s.audio_output = AudioOutput::Disabled;
        assert_eq!(
            s.disabled_reason(LauncherField::AudioChannelMode),
            Some("off")
        );
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
