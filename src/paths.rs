// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-user data locations, following the platform's config-directory
//! conventions without pulling in a dependency.
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

/// Copperline's per-user configuration directory: `$XDG_CONFIG_HOME/copperline`,
/// `%APPDATA%\copperline`, or `$HOME/.config/copperline`, whichever the host
/// offers first. Not created here -- writers call `ensure_parent`.
pub fn config_dir() -> Option<PathBuf> {
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

    /// The cascade is read through `envcfg`, which snapshots the environment
    /// once per process, so this asserts the shape of whatever the host
    /// offered rather than driving each branch with a mutated environment.
    #[test]
    fn per_user_paths_hang_off_one_copperline_directory() {
        let Some(dir) = config_dir() else {
            return; // A host with no HOME/APPDATA/XDG_CONFIG_HOME.
        };
        assert_eq!(dir.file_name().unwrap(), "copperline");
        assert_eq!(
            config_file("gamepads.toml").unwrap(),
            dir.join("gamepads.toml")
        );
        assert_eq!(state_slot_dir().unwrap(), dir.join("states"));
    }
}
