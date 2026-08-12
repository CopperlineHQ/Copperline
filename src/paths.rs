// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-data locations, following the platform's config-directory conventions
//! without pulling in a dependency. An empty `portable.txt` beside the
//! executable opts into keeping the same data beside the executable instead.
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

/// Return the directory containing `executable` when it carries the portable
/// marker. Split out so the filesystem contract can be tested without trying
/// to change the process executable.
fn portable_config_dir(executable: &std::path::Path) -> Option<PathBuf> {
    let dir = executable.parent()?;
    dir.join(PORTABLE_MARKER)
        .is_file()
        .then(|| dir.to_path_buf())
}

/// Copperline's host-data directory. An empty `portable.txt` beside the
/// executable selects that directory; otherwise this is
/// `$XDG_CONFIG_HOME/copperline`, `%APPDATA%\copperline`, or
/// `$HOME/.config/copperline`, whichever the host offers first.
///
/// Not created here -- writers call [`ensure_parent`].
pub fn config_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(discover_config_dir).clone()
}

fn discover_config_dir() -> Option<PathBuf> {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(dir) = portable_config_dir(&executable) {
            return Some(dir);
        }
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
    whdload_dir().map(|dir| dir.join("support"))
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

/// The scanned library, when `[whdload] library_db` does not say otherwise.
/// Beside the support archives, which is the other thing under `whdload/`
/// that Copperline rather than the guest put there.
pub fn whdload_library_db() -> Option<PathBuf> {
    whdload_support_dir().map(|dir| dir.join("launcher.db"))
}

/// What a scan downloaded, when `[whdload] library_cache` does not say
/// otherwise. Safe to delete: it is rebuilt by the next scan.
pub fn whdload_library_cache() -> Option<PathBuf> {
    whdload_support_dir().map(|dir| dir.join("cache"))
}

/// Directory holding the numbered save-state slots.
pub fn state_slot_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("states"))
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

    #[test]
    fn portable_marker_selects_the_executable_directory() {
        let root = ScratchDir::new("portable-marker");
        let executable = root.0.join("copperline.exe");

        assert_eq!(portable_config_dir(&executable), None);
        std::fs::write(root.0.join(PORTABLE_MARKER), []).unwrap();
        assert_eq!(portable_config_dir(&executable), Some(root.0.clone()));
        assert_eq!(
            portable_config_dir(&executable).map(|dir| dir.join("states")),
            Some(root.0.join("states"))
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
