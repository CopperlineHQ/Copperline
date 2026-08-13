// SPDX-License-Identifier: GPL-3.0-or-later

//! Where Copperline puts what it produces, and where its file dialogs start.
//!
//! A host preference, kept in `paths.toml` beside `keymap.toml` and
//! `gamepads.toml` rather than in a machine TOML. Which chipset a machine
//! has is a property of that machine and travels with its config; which
//! folder *this* person keeps screenshots in is not, and a machine config
//! carrying it would stop being something you can hand to someone else.
//!
//! Every entry is optional and every unset entry inherits, so the file is
//! only ever as long as the person writing it wants it to be. An empty
//! `paths.toml`, or none at all, behaves exactly as Copperline does today.
//!
//! Relative entries are taken from [`base`](Paths::base), which is itself
//! taken from the host-data directory when it is relative or unset. That is
//! what makes the whole tree move together: a portable install sets nothing
//! and everything follows the marker, because nothing ever said where it
//! was in absolute terms.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

/// The file's name in the host-data directory.
pub const FILE: &str = "paths.toml";

/// Directories Copperline writes into, and the directories its file
/// dialogs open at. Serialised with `skip_serializing_if` throughout so a
/// saved file records only what was actually set -- an inherited entry is
/// absent rather than written out with its computed value, which would
/// freeze today's default into a file that then stops following it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// The directory the rest are taken from. Unset, or relative, means
    /// the host-data directory -- so portable installs need say nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<PathBuf>,

    // --- what Copperline writes -------------------------------------
    /// Numbered save-state slots and any state saved outside them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub states: Option<PathBuf>,
    /// Screenshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshots: Option<PathBuf>,
    /// Video captures and recorded input scripts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recordings: Option<PathBuf>,
    /// Battery-backed RAMs: the RP5C01's, and the CD32's Akiko NVRAM,
    /// which holds real game saves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nvram: Option<PathBuf>,
    /// Debugger instruction traces and waveform captures. A waveform can
    /// reach half a gigabyte, which is its own reason to know where it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traces: Option<PathBuf>,
    /// Machine configurations saved from the launcher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configs: Option<PathBuf>,

    // --- where the file dialogs start -------------------------------
    //
    // These are not Copperline's to write. A dialog already opens beside
    // whatever is in the field it was launched from, which is better than
    // any fixed answer; these only decide where it opens when that field
    // is empty. Nothing is created for them, because a handful of empty
    // folders nobody asked for is worse than none.
    /// Kickstart and extended ROM images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roms: Option<PathBuf>,
    /// MT-32 control and PCM ROMs, under the ROMs directory by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mt32_roms: Option<PathBuf>,
    /// Floppy images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub floppies: Option<PathBuf>,
    /// Hard-disk images and host-filesystem folders.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harddrives: Option<PathBuf>,
    /// CD images.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cds: Option<PathBuf>,
}

/// The name each entry carries when unset. Stated once so the defaults, the
/// resolver and anything that lists them cannot disagree.
mod default_name {
    pub const STATES: &str = "states";
    pub const SCREENSHOTS: &str = "screenshots";
    pub const RECORDINGS: &str = "recordings";
    pub const NVRAM: &str = "nvram";
    pub const TRACES: &str = "traces";
    pub const CONFIGS: &str = "configs";
    pub const ROMS: &str = "roms";
    pub const MT32: &str = "mt32";
    pub const FLOPPIES: &str = "floppies";
    pub const HARDDRIVES: &str = "harddrives";
    pub const CDS: &str = "cds";
}

impl Paths {
    /// Read `paths.toml`, or the defaults when there is none.
    ///
    /// A malformed file is reported and then ignored. It names only where
    /// things go, so falling back to the defaults leaves a working
    /// emulator; refusing to start over it would be a poor trade, and the
    /// message says which file to look at.
    pub fn load() -> Self {
        let Some(path) = crate::paths::config_file(FILE) else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&text) {
            Ok(paths) => paths,
            Err(err) => {
                log::warn!("{}: {err}; using the default directories", path.display());
                Self::default()
            }
        }
    }

    /// Write the entries that were set.
    pub fn save(&self) -> Result<()> {
        let path = crate::paths::config_file(FILE)
            .ok_or_else(|| anyhow!("no host-data directory for {FILE}"))?;
        crate::paths::ensure_parent(&path)?;
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        log::info!("saved directories to {}", path.display());
        Ok(())
    }

    /// The directory everything else is taken from: `base` when it is
    /// absolute, `base` under the host-data directory when it is relative,
    /// and the host-data directory itself when it is unset.
    pub fn base_dir(&self, host_data: &Path) -> PathBuf {
        match self.base.as_deref() {
            Some(base) if base.is_absolute() => base.to_path_buf(),
            Some(base) => host_data.join(base),
            None => host_data.to_path_buf(),
        }
    }

    /// One entry, resolved: absolute as given, relative under the base, and
    /// the stated default name under the base when unset.
    fn dir(&self, host_data: &Path, set: Option<&Path>, default: &str) -> PathBuf {
        match set {
            Some(dir) if dir.is_absolute() => dir.to_path_buf(),
            Some(dir) => self.base_dir(host_data).join(dir),
            None => self.base_dir(host_data).join(default),
        }
    }

    /// Save-state slots.
    pub fn states_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.states.as_deref(), default_name::STATES)
    }

    /// Screenshots.
    pub fn screenshots_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(
            host_data,
            self.screenshots.as_deref(),
            default_name::SCREENSHOTS,
        )
    }

    /// Video and input recordings.
    pub fn recordings_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(
            host_data,
            self.recordings.as_deref(),
            default_name::RECORDINGS,
        )
    }

    /// Battery-backed RAMs.
    pub fn nvram_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.nvram.as_deref(), default_name::NVRAM)
    }

    /// Traces and waveform captures.
    pub fn traces_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.traces.as_deref(), default_name::TRACES)
    }

    /// Saved machine configurations.
    pub fn configs_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.configs.as_deref(), default_name::CONFIGS)
    }

    /// Where a ROM dialog opens when its field is empty.
    pub fn roms_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.roms.as_deref(), default_name::ROMS)
    }

    /// Where an MT-32 ROM dialog opens. Under the ROMs directory rather
    /// than beside it: they are ROMs, just not the machine's own.
    pub fn mt32_roms_dir(&self, host_data: &Path) -> PathBuf {
        match self.mt32_roms.as_deref() {
            Some(dir) if dir.is_absolute() => dir.to_path_buf(),
            Some(dir) => self.base_dir(host_data).join(dir),
            None => self.roms_dir(host_data).join(default_name::MT32),
        }
    }

    /// Where a floppy dialog opens when its field is empty.
    pub fn floppies_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.floppies.as_deref(), default_name::FLOPPIES)
    }

    /// Where a hard-disk dialog opens when its field is empty.
    pub fn harddrives_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(
            host_data,
            self.harddrives.as_deref(),
            default_name::HARDDRIVES,
        )
    }

    /// Where a CD dialog opens when its field is empty.
    pub fn cds_dir(&self, host_data: &Path) -> PathBuf {
        self.dir(host_data, self.cds.as_deref(), default_name::CDS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> PathBuf {
        PathBuf::from("/host/copperline")
    }

    #[test]
    fn nothing_set_puts_everything_under_the_host_directory() {
        let p = Paths::default();
        let h = host();
        assert_eq!(p.base_dir(&h), h);
        assert_eq!(p.states_dir(&h), h.join("states"));
        assert_eq!(p.screenshots_dir(&h), h.join("screenshots"));
        assert_eq!(p.recordings_dir(&h), h.join("recordings"));
        assert_eq!(p.nvram_dir(&h), h.join("nvram"));
        assert_eq!(p.traces_dir(&h), h.join("traces"));
        assert_eq!(p.configs_dir(&h), h.join("configs"));
        assert_eq!(p.roms_dir(&h), h.join("roms"));
        assert_eq!(p.mt32_roms_dir(&h), h.join("roms").join("mt32"));
        assert_eq!(p.floppies_dir(&h), h.join("floppies"));
        assert_eq!(p.harddrives_dir(&h), h.join("harddrives"));
        assert_eq!(p.cds_dir(&h), h.join("cds"));
    }

    /// The property the whole design rests on: nothing records where it is
    /// in absolute terms, so moving the root moves everything with it. This
    /// is what makes a portable install a folder copy and not a migration.
    #[test]
    fn the_whole_tree_follows_the_root() {
        let p = Paths::default();
        let stick = PathBuf::from("/Volumes/STICK/Copperline");
        for (a, b) in [
            (p.states_dir(&host()), p.states_dir(&stick)),
            (p.screenshots_dir(&host()), p.screenshots_dir(&stick)),
            (p.nvram_dir(&host()), p.nvram_dir(&stick)),
            (p.mt32_roms_dir(&host()), p.mt32_roms_dir(&stick)),
        ] {
            assert!(a.starts_with(host()), "{a:?} left the host root");
            assert!(b.starts_with(&stick), "{b:?} did not follow the root");
            assert_eq!(
                a.strip_prefix(host()),
                b.strip_prefix(&stick),
                "the same entry sits differently under two roots"
            );
        }
    }

    #[test]
    fn a_relative_entry_hangs_off_the_base_and_an_absolute_one_does_not() {
        let h = host();
        let p = Paths {
            screenshots: Some(PathBuf::from("shots")),
            recordings: Some(PathBuf::from("/mnt/scratch/video")),
            ..Default::default()
        };
        assert_eq!(p.screenshots_dir(&h), h.join("shots"));
        assert_eq!(p.recordings_dir(&h), PathBuf::from("/mnt/scratch/video"));
    }

    #[test]
    fn base_moves_the_lot_without_naming_each_one() {
        let h = host();
        let p = Paths {
            base: Some(PathBuf::from("/data/copperline")),
            ..Default::default()
        };
        assert_eq!(p.states_dir(&h), PathBuf::from("/data/copperline/states"));
        assert_eq!(p.nvram_dir(&h), PathBuf::from("/data/copperline/nvram"));
        // A relative base is still taken from the host directory, so it
        // cannot escape a portable root by accident.
        let rel = Paths {
            base: Some(PathBuf::from("data")),
            ..Default::default()
        };
        assert_eq!(rel.states_dir(&h), h.join("data").join("states"));
    }

    /// An inherited entry must not be written out with its computed value:
    /// a file that recorded today's defaults would stop following them, and
    /// the person who wrote it never asked for that.
    #[test]
    fn saving_records_only_what_was_set() {
        let p = Paths {
            screenshots: Some(PathBuf::from("shots")),
            ..Default::default()
        };
        let text = toml::to_string_pretty(&p).unwrap();
        assert!(text.contains("screenshots"), "{text}");
        for absent in ["states", "nvram", "traces", "roms", "base"] {
            assert!(!text.contains(absent), "{absent} written out: {text}");
        }
        assert_eq!(toml::from_str::<Paths>(&text).unwrap(), p);
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(toml::from_str::<Paths>("").unwrap(), Paths::default());
    }
}
