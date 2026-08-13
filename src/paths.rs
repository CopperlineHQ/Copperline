// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-data locations, following the platform's config-directory conventions
//! without pulling in a dependency. An empty `portable.txt` beside the
//! executable (or downloaded AppImage) opts into keeping the same data there
//! instead.
//!
//! These are *host preferences and per-user data* -- gamepad calibration,
//! keyboard mappings, save-state slots -- not machine configuration. Anything
//! that describes the emulated machine belongs in a config TOML the user
//! points at explicitly, so it stays portable and reviewable; anything that
//! describes this host's setup belongs here, so it survives switching between
//! machine configs.
//!
//! Everything returns `Option`: a host with none of the environment variables
//! set (a bare service account, some CI runners) simply has no per-user
//! directory, and every caller degrades to "not persisted" rather than
//! failing.

use std::path::PathBuf;

/// Marker that opts an installation into keeping host data beside the
/// executable. Its contents are deliberately ignored: existence is the whole
/// switch, so portable archives need no machine-specific configuration.
pub const PORTABLE_MARKER: &str = "portable.txt";

/// Return the directory containing `program` when it carries the portable
/// marker.
fn marked_program_dir(program: &std::path::Path) -> Option<PathBuf> {
    let dir = program.parent()?;
    dir.join(PORTABLE_MARKER)
        .is_file()
        .then(|| dir.to_path_buf())
}

/// Resolve portable mode from the program path the user launched. An AppImage
/// executes its embedded binary from a temporary read-only mount, but the
/// runtime exposes the downloaded image as `APPIMAGE`; that original path must
/// win so a marker can sit beside the image and the chosen directory is
/// writable. Split out so this ordering can be tested without changing the
/// process environment or executable.
fn portable_config_dir(
    appimage: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
) -> Option<PathBuf> {
    appimage
        .into_iter()
        .chain(executable)
        .find_map(marked_program_dir)
}

/// Copperline's host-data directory. An empty `portable.txt` beside the
/// executable or downloaded AppImage selects that directory; otherwise this is
/// `$XDG_CONFIG_HOME/copperline`, `%APPDATA%\copperline`, or
/// `$HOME/.config/copperline`, whichever the host offers first.
///
/// Not created here -- writers call [`ensure_parent`].
pub fn config_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(discover_config_dir).clone()
}

fn discover_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    let appimage = crate::envcfg::var_os("APPIMAGE").map(PathBuf::from);
    #[cfg(not(target_os = "linux"))]
    let appimage: Option<PathBuf> = None;
    let executable = std::env::current_exe().ok();
    if let Some(dir) = portable_config_dir(appimage.as_deref(), executable.as_deref()) {
        return Some(dir);
    }
    for var in ["XDG_CONFIG_HOME", "APPDATA"] {
        if let Some(dir) = crate::envcfg::var_os(var) {
            return Some(PathBuf::from(dir).join("copperline"));
        }
    }
    crate::envcfg::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("copperline"))
}

/// A named file directly inside [`config_dir`], e.g. `gamepads.toml`.
pub fn config_file(name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(name))
}

/// Where WHDLoad keeps everything of its own: the support archives, the
/// extracted games and their saves, and the game database.
///
/// One place, named once. Every WHDLoad setting that says `(default)`
/// means somewhere under here, so a person who never sets any of them
/// still knows where to look, and a person who moves one knows what they
/// are moving it away from.
pub fn whdload_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("whdload"))
}

/// Where the support archives are looked for and downloaded to.
pub fn whdload_support_dir() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_support_in(&dir))
}

/// Where games are unpacked and their saves kept, when `[whdload] library`
/// does not say otherwise. Beside the support archives rather than loose
/// in `whdload/`, so what Copperline downloaded and what the guest wrote
/// are told apart at a glance.
///
/// An installation that already has games directly under `whdload/` --
/// where this used to be -- carries on using it. The library directory is
/// where saves live, so moving it would leave somebody's savegames and
/// highscores behind under a name nothing looks at any more.
pub fn whdload_save_dir() -> Option<PathBuf> {
    let whdload = whdload_dir()?;
    let save = whdload.join("save");
    if save.is_dir() || !holds_unpacked_games(&whdload) {
        return Some(save);
    }
    log::info!(
        "whdload: keeping the existing game library in {} (new installations use {})",
        whdload.display(),
        save.display()
    );
    Some(whdload)
}

/// Whether a directory holds games unpacked by an earlier version: an
/// entry with the `.source` marker staging writes beside each one.
fn holds_unpacked_games(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join(".source").is_file())
}

// The same layout, spelled from a root the caller chose. The launcher is
// handed a root rather than asking for one, so a test can point it
// somewhere harmless -- and it had grown its own copy of these joins,
// which meant the tree Copperline writes and the tree `paths` described
// were two independent claims that happened to agree.

/// The support directory under a given root.
pub fn whdload_support_in(root: &std::path::Path) -> PathBuf {
    root.join("whdload").join("support")
}

/// The scanned library under a given root.
pub fn whdload_library_db_in(root: &std::path::Path) -> PathBuf {
    whdload_support_in(root).join("launcher.db")
}

/// A scan's downloads under a given root.
pub fn whdload_library_cache_in(root: &std::path::Path) -> PathBuf {
    whdload_support_in(root).join("cache")
}

/// The scanned library, when `[whdload] library_db` does not say otherwise.
/// Beside the support archives, which is the other thing under `whdload/`
/// that Copperline rather than the guest put there.
pub fn whdload_library_db() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_library_db_in(&dir))
}

/// What a scan downloaded, when `[whdload] library_cache` does not say
/// otherwise. Safe to delete: it is rebuilt by the next scan.
pub fn whdload_library_cache() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_library_cache_in(&dir))
}

/// The root the launcher takes its library paths from.
///
/// A host with no per-user directory at all -- no `HOME`, no `XDG_CONFIG_HOME`,
/// no `APPDATA`, which is a bare service account or some CI runners -- has
/// nowhere to put these, and every call site reached for
/// `config_dir().unwrap_or_default()`. That yields an *empty* path, so the
/// library quietly becomes `whdload/support/launcher.db` relative to
/// wherever the process was started. Named here rather than repeated at
/// eight call sites, with the behaviour unchanged: it is a poor answer, but
/// changing it is a decision about where data lives and not about who owns
/// the path.
pub fn library_root() -> PathBuf {
    config_dir().unwrap_or_default()
}

/// Directory holding the numbered save-state slots.
pub fn state_slot_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dirs().states_dir(&dir))
}

// --- what a run produces ------------------------------------------------
//
// Screenshots, recordings, save states, traces, waveform captures and the
// battery-backed RAMs. Each of these had its name and its location written
// out at the point it was used -- in nine places, two of them identical --
// so a file's home depended on which code path produced it and no one
// place knew the whole set.
//
// They are gathered here unchanged: every function below still resolves to
// exactly where its caller put things before, which for most of them is
// the process's working directory. That is not where they belong, but
// moving them and re-homing them at once would make a regression in the
// routing indistinguishable from an argument about the destination. With
// one owner, the move is an edit to these bodies.

// Two stamp formats are in use and both are kept. What a person opens --
// screenshots, recordings, states -- is named with a readable local
// datetime; the diagnostic captures are named with Unix seconds. Nobody
// chose that split, but a filename is what a script greps and a person
// recognises, so unifying it is a change in its own right and not one to
// smuggle in here.

/// The configured directories, read once. `paths.toml` says where each of
/// these goes; absent or empty, it says nothing and the defaults stand.
fn dirs() -> &'static crate::pathconf::Paths {
    static DIRS: std::sync::OnceLock<crate::pathconf::Paths> = std::sync::OnceLock::new();
    DIRS.get_or_init(crate::pathconf::Paths::load)
}

/// A named file under one of the configured directories.
///
/// A host with no per-user directory at all keeps the bare name, which is
/// the working directory -- the same "degrade to not persisted" the rest of
/// this module promises, rather than an error at the point somebody presses
/// the screenshot key.
fn output(
    pick: impl Fn(&crate::pathconf::Paths, &std::path::Path) -> PathBuf,
    name: String,
) -> PathBuf {
    match config_dir() {
        Some(host) => pick(dirs(), &host).join(name),
        None => PathBuf::from(name),
    }
}

/// Seconds since the epoch, for the diagnostic captures.
fn epoch_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Default name for a screenshot taken without one being given.
pub fn screenshot_file() -> PathBuf {
    output(
        |p, h| p.screenshots_dir(h),
        format!(
            "copperline-screenshot-{}.png",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a video recording.
pub fn recording_file() -> PathBuf {
    output(
        |p, h| p.recordings_dir(h),
        format!("copperline-video-{}.avi", crate::timestamp::compact_now()),
    )
}

/// Default name for a recorded input script.
pub fn input_recording_file() -> PathBuf {
    output(
        |p, h| p.recordings_dir(h),
        format!(
            "copperline-input-{}.clscript",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a save state written outside the numbered slots.
pub fn state_file() -> PathBuf {
    output(
        |p, h| p.states_dir(h),
        format!(
            "copperline-state-{}.clstate",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a waveform capture. Reached from `--waveform` without a
/// path and from `waveform.start` without one; a capture can run to half a
/// gigabyte, which is its own argument against the working directory.
pub fn waveform_file() -> PathBuf {
    output(
        |p, h| p.traces_dir(h),
        format!("copperline-wave-{}.vcd", epoch_stamp()),
    )
}

/// Default name for an instruction trace. The debugger console and the
/// control protocol both start traces, and each had its own copy of this;
/// they now cannot drift.
pub fn trace_file() -> PathBuf {
    output(
        |p, h| p.traces_dir(h),
        format!("copperline-trace-{}.txt", epoch_stamp()),
    )
}

/// A battery-backed RAM, under the configured directory -- unless one is
/// already sitting where Copperline used to put it.
///
/// These held their bare names, so they landed in whatever directory the
/// process happened to start in. Moving where Copperline *looks* is not the
/// same as moving the file: the CD32's is a memory card holding real game
/// saves, and a player whose progress silently reverted to blank would have
/// no way of knowing it was still on disk a directory away. So an existing
/// file keeps being used where it is, and only a machine that never had one
/// gets the new place. Nothing is moved behind anyone's back.
fn battery_ram(name: &str) -> PathBuf {
    let legacy = PathBuf::from(name);
    if legacy.is_file() {
        log::info!(
            "using the existing {name} in the working directory; new machines keep theirs under the host data directory"
        );
        return legacy;
    }
    match config_dir() {
        Some(host) => dirs().nvram_dir(&host).join(name),
        None => legacy,
    }
}

/// The RP5C01's battery-backed RAM, when `[machine] battmem` does not say
/// otherwise. Fitted to A3000/A4000-class machines.
pub fn battery_ram_file() -> PathBuf {
    battery_ram("battmem.nvram")
}

/// The CD32's Akiko NVRAM, when nothing else says otherwise.
pub fn akiko_nvram_file() -> PathBuf {
    battery_ram("cd32-nvram.bin")
}

/// Create a path's parent directory so a write to it can succeed.
pub fn ensure_parent(path: &std::path::Path) -> std::io::Result<()> {
    match path.parent() {
        Some(parent) => std::fs::create_dir_all(parent),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("copperline-paths-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The names a run produces, and which stamp each carries. Gathering
    /// these into one place is only safe if none of them changed on the way
    /// -- a filename is what a person recognises and a script greps -- and
    /// the two stamp formats are easy to swap by accident, because nothing
    /// but the call site said which was which.
    #[test]
    fn the_output_names_keep_their_shape_and_their_stamp() {
        let readable = |p: &std::path::Path, prefix: &str, ext: &str| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            assert!(name.starts_with(prefix), "{name} does not start {prefix}");
            assert!(name.ends_with(ext), "{name} does not end {ext}");
            let stamp = &name[prefix.len()..name.len() - ext.len()];
            // yyyymmddHHMMSS: a local datetime a person can read.
            assert_eq!(stamp.len(), 14, "{name}: not a compact datetime");
            assert!(stamp.chars().all(|c| c.is_ascii_digit()), "{name}");
            assert!(stamp.starts_with("20"), "{name}: not a year");
        };
        readable(&screenshot_file(), "copperline-screenshot-", ".png");
        readable(&recording_file(), "copperline-video-", ".avi");
        readable(&input_recording_file(), "copperline-input-", ".clscript");
        readable(&state_file(), "copperline-state-", ".clstate");

        let epoch = |p: &std::path::Path, prefix: &str, ext: &str| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            assert!(name.starts_with(prefix), "{name} does not start {prefix}");
            assert!(name.ends_with(ext), "{name} does not end {ext}");
            let stamp = &name[prefix.len()..name.len() - ext.len()];
            let secs: u64 = stamp.parse().unwrap_or_else(|_| panic!("{name}"));
            // Unix seconds, not a datetime: far past any 14-digit value.
            assert!(secs > 1_600_000_000, "{name}: not unix seconds");
            assert!(secs < 4_000_000_000, "{name}: not unix seconds");
        };
        epoch(&waveform_file(), "copperline-wave-", ".vcd");
        epoch(&trace_file(), "copperline-trace-", ".txt");

        // The two battery RAMs are fixed names, not stamped. Where they
        // sit is `nvram_dir`'s business and is covered separately; what
        // matters here is that the names themselves never changed.
        assert_eq!(
            battery_ram_file().file_name().and_then(|n| n.to_str()),
            Some("battmem.nvram")
        );
        assert_eq!(
            akiko_nvram_file().file_name().and_then(|n| n.to_str()),
            Some("cd32-nvram.bin")
        );
    }

    /// The launcher is handed a root and spells the library paths from it;
    /// the no-argument helpers spell them from the config directory. Those
    /// were two independent descriptions of the same tree, agreeing only by
    /// coincidence, so hold them to each other.
    /// A battery RAM sitting where Copperline used to put it keeps being
    /// used from there. The CD32's is a memory card holding real game
    /// saves; looking somewhere new would present a player with a blank
    /// card and no hint that their progress was still on disk.
    #[test]
    fn an_existing_battery_ram_is_not_abandoned() {
        let scratch = ScratchDir::new("battery");
        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&scratch.0).unwrap();

        // Nothing there: the new place, under the host data directory.
        let fresh = akiko_nvram_file();
        assert!(
            fresh.parent().is_some_and(|p| p.ends_with("nvram")) || config_dir().is_none(),
            "a machine with no card should get the nvram directory: {fresh:?}"
        );

        // One there: that one, wherever it is.
        std::fs::write("cd32-nvram.bin", []).unwrap();
        assert_eq!(akiko_nvram_file(), PathBuf::from("cd32-nvram.bin"));
        std::fs::write("battmem.nvram", []).unwrap();
        assert_eq!(battery_ram_file(), PathBuf::from("battmem.nvram"));

        std::env::set_current_dir(here).unwrap();
    }

    #[test]
    fn the_library_layout_is_one_description() {
        let root = std::path::Path::new("/tmp/copperline-root");
        assert_eq!(
            whdload_library_db_in(root),
            whdload_support_in(root).join("launcher.db")
        );
        assert_eq!(
            whdload_library_cache_in(root),
            whdload_support_in(root).join("cache")
        );
        assert_eq!(
            whdload_support_in(root),
            root.join("whdload").join("support")
        );
        // And the no-argument forms are the same layout under the config
        // directory, whatever that turns out to be on this host.
        if let Some(dir) = config_dir() {
            assert_eq!(whdload_support_dir(), Some(whdload_support_in(&dir)));
            assert_eq!(whdload_library_db(), Some(whdload_library_db_in(&dir)));
            assert_eq!(
                whdload_library_cache(),
                Some(whdload_library_cache_in(&dir))
            );
        }
    }

    #[test]
    fn portable_marker_selects_the_executable_directory() {
        let root = ScratchDir::new("portable-marker");
        let executable = root.0.join("copperline.exe");

        assert_eq!(portable_config_dir(None, Some(&executable)), None);
        std::fs::write(root.0.join(PORTABLE_MARKER), []).unwrap();
        assert_eq!(
            portable_config_dir(None, Some(&executable)),
            Some(root.0.clone())
        );
        assert_eq!(
            portable_config_dir(None, Some(&executable)).map(|dir| dir.join("states")),
            Some(root.0.join("states"))
        );
    }

    #[test]
    fn appimage_marker_selects_the_download_location_before_the_mounted_binary() {
        let root = ScratchDir::new("appimage-marker");
        let download = root.0.join("download");
        let mount = root.0.join("mount/usr/bin");
        std::fs::create_dir_all(&download).unwrap();
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(download.join(PORTABLE_MARKER), []).unwrap();

        let appimage = download.join("Copperline.AppImage");
        let executable = mount.join("copperline");
        assert_eq!(
            portable_config_dir(Some(&appimage), Some(&executable)),
            Some(download)
        );
    }

    /// The cascade is read through `envcfg`, which snapshots the environment
    /// once per process, so this asserts the shape of whatever the host
    /// offered rather than driving each branch with a mutated environment.
    #[test]
    fn host_data_paths_hang_off_one_directory() {
        let Some(dir) = config_dir() else {
            return; // A host with no HOME/APPDATA/XDG_CONFIG_HOME.
        };
        assert_eq!(
            config_file("gamepads.toml").unwrap(),
            dir.join("gamepads.toml")
        );
        assert_eq!(state_slot_dir().unwrap(), dir.join("states"));
    }
}
