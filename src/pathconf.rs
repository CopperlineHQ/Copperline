// SPDX-License-Identifier: GPL-3.0-or-later

//! Where Copperline puts what it produces, and where its file dialogs start.
//!
//! The `[paths]` section of a configuration, and no file of its own: one
//! TOML says what the machine is and where its output goes, which is one
//! thing to keep, copy or hand over rather than two that have to be kept
//! in step.
//!
//! Every entry is optional and every unset entry inherits, so the section
//! only exists at all once somebody moves something. No `[paths]`, or an
//! empty one, behaves exactly as Copperline does with no configuration.
//!
//! Relative entries are taken from [`base`](Paths::base), which is itself
//! taken from the host-data directory when it is relative or unset. That is
//! what makes the whole tree move together: a portable install sets nothing
//! and everything follows the marker, because nothing ever said where it
//! was in absolute terms.
//!
//! A configuration written on one machine will happily name directories the
//! next one has never heard of, so [`Paths::reachable`] drops the entries
//! whose directories are not there and lets them inherit. It is bounded in
//! time as well as in work: a directory on a network share that has gone
//! away does not fail, it *hangs*, and Copperline starting is not something
//! to make conditional on a file server answering.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How long the whole reachability check may take before Copperline stops
/// waiting and inherits the rest. Long enough that a busy disk still
/// answers, short enough that a stale mount is a pause and not a hang.
const PROBE_BUDGET: Duration = Duration::from_millis(1500);

/// Directories Copperline writes into, and the directories its file
/// dialogs open at. Serialised with `skip_serializing_if` throughout so a
/// saved configuration records only what was actually set -- an inherited
/// entry is absent rather than written out with its computed value, which
/// would freeze today's default into a file that then stops following it.
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
    /// The one entry with no row on the Paths page: they are ROMs, they
    /// follow the ROMs directory, and a second ROM row for the handful of
    /// people who keep them apart is a poor trade against a shorter page.
    /// Still read and still written back, so a configuration that sets it
    /// keeps it.
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

/// Whether a directory is one Copperline can use: it is there, or it is
/// inside `root`, which is Copperline's own tree and made on demand.
///
/// The second half is what makes a fresh installation work. Nothing under
/// the host-data directory exists until something is written to it, so a
/// check that insisted on finding `screenshots = "shots"` -- or the
/// host-data directory containing it -- would drop every relative entry on
/// a machine that had not run Copperline before. Everything inside the root
/// is created with the write that needs it, so being missing says nothing.
///
/// Outside the root, existing is the whole test. `/Volumes/STICK/Copperline`
/// with the stick unplugged is not there and inherits, rather than being
/// treated as creatable and quietly built as a shadow copy of somebody's
/// library on the internal disk.
///
/// One `stat` at most, and only for a path outside the root -- the inside
/// case is a lexical prefix test that touches no disk and cannot block.
/// Nothing is created here; whether the eventual write succeeds is the
/// write's business to report.
fn is_reachable(dir: &Path, root: &Path) -> bool {
    dir.starts_with(root) || dir.is_dir()
}

/// Check each directory, giving up at `deadline`.
///
/// The checking runs on a thread of its own because it cannot be
/// interrupted: a `stat` into a network mount whose server has gone away
/// blocks in the kernel until it decides to time out, which on a default
/// NFS mount is minutes. Verdicts come back one at a time as they are
/// reached, so a hang costs only the entries behind it -- everything
/// already answered still counts. Anything unanswered when the deadline
/// passes is reported unreachable, and the thread is abandoned: it holds no
/// lock, touches nothing shared, and ends when the kernel lets it.
fn probe_within(dirs: &[PathBuf], root: &Path, deadline: Instant) -> Vec<bool> {
    if dirs.is_empty() {
        return Vec::new();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let owned: Vec<PathBuf> = dirs.to_vec();
    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("paths-probe".to_string())
        .spawn(move || {
            for dir in owned {
                // A closed channel means the deadline passed and nobody is
                // listening any more; there is nothing left to do.
                if tx.send(is_reachable(&dir, &root)).is_err() {
                    return;
                }
            }
        })
        // A host that cannot spawn a thread has bigger problems than where
        // its screenshots go; inherit everything and carry on.
        .map_or_else(
            |_| vec![false; dirs.len()],
            |_| {
                let mut verdicts = Vec::with_capacity(dirs.len());
                while verdicts.len() < dirs.len() {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match rx.recv_timeout(left) {
                        Ok(verdict) => verdicts.push(verdict),
                        Err(_) => break,
                    }
                }
                verdicts.resize(dirs.len(), false);
                verdicts
            },
        )
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
    /// Whether anything at all was set. Nothing set is the overwhelmingly
    /// common case and costs nothing to answer, which is what keeps the
    /// reachability check off the startup path for almost everybody.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Every entry but the base, destructured rather than listed so a field
    /// added later cannot quietly escape the check below.
    fn entries(&mut self) -> [&mut Option<PathBuf>; 11] {
        let Paths {
            base: _,
            states,
            screenshots,
            recordings,
            nvram,
            traces,
            configs,
            roms,
            mt32_roms,
            floppies,
            harddrives,
            cds,
        } = self;
        [
            states,
            screenshots,
            recordings,
            nvram,
            traces,
            configs,
            roms,
            mt32_roms,
            floppies,
            harddrives,
            cds,
        ]
    }

    /// Drop the entries whose directories are not there, so what is left
    /// inherits.
    ///
    /// A configuration is a portable thing: it gets copied between machines,
    /// checked in, and handed to other people, and the memory stick one of
    /// them named is not going to be plugged into the next one. Rather than
    /// fail, or write somewhere surprising, an entry Copperline cannot reach
    /// stops applying and that directory goes back to its default.
    ///
    /// "There" means the directory itself, or the directory it would be
    /// created inside: Copperline makes its own output directories on first
    /// write, so naming one that does not exist yet is normal and only its
    /// parent has to be real.
    ///
    /// Bounded in time. `stat` on a network share whose server has gone away
    /// blocks in the kernel and cannot be cancelled, so the probing happens
    /// on a thread of its own and this waits [`PROBE_BUDGET`] for it.
    /// Whatever has not answered by then is treated as unreachable and
    /// inherits -- the wrong-but-working answer, arrived at promptly, rather
    /// than the right one at the cost of a startup that never finishes. The
    /// thread is left to end in its own time; it holds nothing.
    pub fn reachable(mut self, host_data: &Path) -> Self {
        if self.is_empty() {
            return self;
        }
        let deadline = Instant::now() + PROBE_BUDGET;
        // The base goes first and on its own. Everything relative hangs off
        // it, so an unreachable base is not one bad entry but a wrong answer
        // for all of them; dropping it first means the rest are then
        // measured where they are actually about to be written.
        if let Some(base) = self.base.clone() {
            let dir = host_data.join(base);
            if !probe_within(std::slice::from_ref(&dir), host_data, deadline)[0] {
                log::warn!("{}: base folder unreachable", dir.display());
                self.base = None;
            }
        }
        let base = self.base_dir(host_data);
        let mut entries = self.entries();
        let resolved: Vec<PathBuf> = entries
            .iter()
            .map(|entry| match entry.as_deref() {
                Some(dir) => base.join(dir),
                // Unset entries are probed too rather than skipped, so the
                // verdicts line up with the entries by position and no
                // index arithmetic stands between the two.
                None => base.clone(),
            })
            .collect();
        let verdicts = probe_within(&resolved, &base, deadline);
        for ((entry, dir), reachable) in entries.iter_mut().zip(resolved).zip(verdicts) {
            if entry.is_some() && !reachable {
                log::warn!("{}: unreachable, using the default instead", dir.display());
                **entry = None;
            }
        }
        self
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
        assert!(Paths::default().is_empty());
    }

    /// The point of the whole check: a configuration written on a machine
    /// with a memory stick plugged in still starts on one without it, with
    /// the entries that *are* there left alone.
    #[test]
    fn an_entry_that_is_not_there_inherits_and_the_rest_do_not() {
        let root = std::env::temp_dir();
        let paths = Paths {
            // A volume that is not mounted: the case the whole check is for.
            screenshots: Some(PathBuf::from("/no-such-volume-9f3a/shots")),
            // A directory that does not exist yet, inside Copperline's own
            // root: normal on a fresh installation, because it is made on
            // the first write.
            states: Some(PathBuf::from("not-yet-created")),
            // One that is simply there.
            recordings: Some(root.clone()),
            ..Default::default()
        }
        .reachable(&root);
        assert_eq!(paths.screenshots, None, "a missing volume should inherit");
        assert!(paths.states.is_some(), "a creatable directory should stand");
        assert!(paths.recordings.is_some(), "an existing one should stand");
    }

    /// An unreachable base takes only itself out: the rest fall back to the
    /// host directory rather than being condemned along with it.
    #[test]
    fn an_unreachable_base_leaves_the_entries_under_the_default_root() {
        let paths = Paths {
            base: Some(PathBuf::from("/no-such-volume-4c1d/Copperline")),
            screenshots: Some(std::env::temp_dir()),
            ..Default::default()
        }
        .reachable(&host());
        assert_eq!(paths.base, None);
        assert!(paths.screenshots.is_some());
    }

    /// A fresh installation has written nothing yet, so nothing under the
    /// host-data directory is there -- including the host-data directory.
    /// Relative entries must still stand: they name folders Copperline
    /// makes when it writes to them, and dropping them would mean `[paths]`
    /// quietly did nothing until the first screenshot had been taken
    /// somewhere else. This is what CI caught and a developer's own machine,
    /// which has all of these directories already, cannot.
    #[test]
    fn a_relative_entry_stands_before_anything_has_been_written() {
        let untouched = std::env::temp_dir().join("copperline-never-created-7b2e");
        assert!(
            !untouched.is_dir(),
            "the test needs a root that is not there"
        );
        let paths = Paths {
            screenshots: Some(PathBuf::from("shots")),
            base: Some(PathBuf::from("tree")),
            ..Default::default()
        }
        .reachable(&untouched);
        assert_eq!(paths.base.as_deref(), Some(Path::new("tree")));
        assert_eq!(paths.screenshots.as_deref(), Some(Path::new("shots")));
    }

    /// Nothing set means nothing to check, so the common case never touches
    /// the disk at all.
    #[test]
    fn nothing_set_is_not_probed() {
        assert_eq!(Paths::default().reachable(&host()), Paths::default());
    }

    /// A probe that never answers costs the budget and no more, and the
    /// entries behind it inherit rather than the whole thing failing.
    #[test]
    fn the_check_gives_up_rather_than_waiting() {
        let started = Instant::now();
        let verdicts = probe_within(
            &[PathBuf::from("/"), PathBuf::from("/")],
            Path::new("/"),
            started - Duration::from_secs(1),
        );
        assert_eq!(verdicts.len(), 2, "one verdict per directory, always");
        assert!(
            started.elapsed() < PROBE_BUDGET,
            "a passed deadline should not be waited out"
        );
    }
}
