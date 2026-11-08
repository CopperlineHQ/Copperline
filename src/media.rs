// SPDX-License-Identifier: GPL-3.0-or-later

//! File classification shared by desktop drops and command-line launching.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Floppy,
    Cd,
    HardDisk,
    Rom,
    WhdloadGame,
    Config,
    /// Callers retain their own fallback: ROM for legacy positional paths,
    /// content-sniffed floppy for files dropped on an emulation window.
    Unknown,
}

/// Extensions route most files. ZIP member names distinguish WHDLoad games
/// from floppy images; CHD metadata distinguishes CDs from hard disks.
pub fn classify(path: &Path) -> MediaKind {
    if path.is_dir() && crate::package::one_game(path) {
        return MediaKind::WhdloadGame;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("toml") => MediaKind::Config,
        Some("chd") if crate::harddrive::chd::is_hard_disk_chd(path) => MediaKind::HardDisk,
        Some("cue" | "iso" | "nrg" | "chd") => MediaKind::Cd,
        Some("hdf" | "hdz" | "vhd" | "img") => MediaKind::HardDisk,
        Some("rom") => MediaKind::Rom,
        Some("zip") => {
            // A slave takes precedence over disk images in the same game.
            // Listing filters macOS metadata without decompressing payloads.
            if crate::package::list(path).is_ok_and(|members| {
                members
                    .iter()
                    .any(|member| crate::package::is_slave_name(&member.to_string_lossy()))
            }) {
                MediaKind::WhdloadGame
            } else {
                MediaKind::Floppy
            }
        }
        Some(ext)
            if crate::package::EXTENSIONS.contains(&ext)
                || crate::package::SLAVE_EXTENSIONS.contains(&ext) =>
        {
            MediaKind::WhdloadGame
        }
        Some(ext) if crate::floppy::IMAGE_EXTENSIONS.contains(&ext) => MediaKind::Floppy,
        _ => MediaKind::Unknown,
    }
}

/// A bare slave names the extracted package containing it. A relative slave
/// in the current directory must become `.` rather than an empty path.
pub fn whdload_game_path(path: PathBuf) -> PathBuf {
    if path
        .file_name()
        .is_some_and(|name| crate::package::is_slave_name(&name.to_string_lossy()))
    {
        return path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_extensions_keep_the_callers_fallback_available() {
        for name in ["kick.bin", "kickstart", "mystery.xyz"] {
            assert_eq!(classify(Path::new(name)), MediaKind::Unknown);
        }
        assert_eq!(classify(Path::new("machine.TOML")), MediaKind::Config);
        assert_eq!(classify(Path::new("disk.ADF")), MediaKind::Floppy);
    }

    #[test]
    fn a_slave_in_the_current_directory_names_that_directory() {
        assert_eq!(whdload_game_path("Game.slave".into()), PathBuf::from("."));
        assert_eq!(
            whdload_game_path("game/Game.SLAV".into()),
            PathBuf::from("game")
        );
    }
}
