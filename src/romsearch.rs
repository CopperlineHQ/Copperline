// SPDX-License-Identifier: GPL-3.0-or-later

//! Locating bundled, freely redistributable ROM assets. AROS supplies the
//! default m68k boot ROM; Copperline's open CD32 FMV image supplies the
//! optional cartridge's default. Unlike Commodore ROMs, both can legally ship
//! alongside the binary under `share/copperline/{aros,fmv}/`.
//!
//! AROS is consumed as two halves, exactly as WinUAE and FS-UAE take it: a
//! 512 KiB main ROM that overlays at $F80000 like any Kickstart, plus a
//! 512 KiB extended ROM mapped at $E00000.

use std::path::{Path, PathBuf};

/// Main (Kickstart-replacement) ROM file name.
pub const AROS_MAIN_FILE: &str = "aros-amiga-m68k-rom.bin";
/// Extended ROM file name (maps at $E00000).
pub const AROS_EXT_FILE: &str = "aros-amiga-m68k-ext.bin";

/// Copperline's freely redistributable CD32 FMV cartridge ROM.
pub const FMV_ROM_FILE: &str = "copperline-fmv.rom";

/// A located pair of bundled AROS ROM files.
pub struct BundledAros {
    pub main: PathBuf,
    pub extended: PathBuf,
}

/// Search the conventional install locations for the bundled AROS ROM pair,
/// returning the first directory that holds both files. The order tried is:
/// an explicit `COPPERLINE_AROS_DIR` override; locations relative to the
/// running executable (a sibling `aros/`, a macOS `.app` `Resources/aros/`,
/// and a Homebrew/Unix `../share/copperline/aros/`); and finally the
/// source-tree `assets/aros/` so `cargo run` works during development.
pub fn find_bundled_aros() -> Option<BundledAros> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_AROS_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("aros"));
            // A macOS .app bundle puts the binary in Contents/MacOS, with
            // data in the sibling Contents/Resources. A Homebrew/Unix install
            // puts it in <prefix>/bin, with data under <prefix>/share.
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("aros"));
                dirs.push(parent.join("share").join("copperline").join("aros"));
            }
        }
    }

    // Development: running straight from the source tree.
    dirs.push(PathBuf::from("assets").join("aros"));

    dirs.into_iter().find_map(|dir| aros_pair_in(&dir))
}

/// When `dir` holds both AROS ROM files, return their paths.
fn aros_pair_in(dir: &Path) -> Option<BundledAros> {
    let main = dir.join(AROS_MAIN_FILE);
    let extended = dir.join(AROS_EXT_FILE);
    (main.is_file() && extended.is_file()).then_some(BundledAros { main, extended })
}

/// Locate the bundled open CD32 FMV ROM. Native packages install it in an
/// `fmv` directory beside the executable's other ROM assets; the environment
/// override keeps development and downstream packaging testable.
pub fn find_bundled_fmv() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_FMV_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("fmv"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("fmv"));
                dirs.push(parent.join("share").join("copperline").join("fmv"));
            }
        }
    }
    dirs.push(PathBuf::from("assets").join("fmv"));

    dirs.into_iter()
        .map(|dir| dir.join(FMV_ROM_FILE))
        .find(|rom| rom.is_file())
}

/// Bundled HRTMon freezer-cartridge image (HRTMon 2.39 assembled from
/// https://github.com/wepl/hrtmon by `hrtmon-rom/build.sh`), used when a
/// config fits the cartridge without naming an image.
pub const HRTMON_ROM_FILE: &str = "hrtmon.rom";

/// Locate the bundled HRTMon image, searching the same places as
/// [`find_bundled_aros`] under an `hrtmon/` subdirectory.
pub fn find_bundled_hrtmon() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_HRTMON_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("hrtmon"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("hrtmon"));
                dirs.push(parent.join("share").join("copperline").join("hrtmon"));
            }
        }
    }
    dirs.push(PathBuf::from("assets").join("hrtmon"));

    dirs.into_iter()
        .map(|dir| dir.join(HRTMON_ROM_FILE))
        .find(|rom| rom.is_file())
}

/// Bundled open-source A4091 autoboot ROM, used when a config fits an A4091
/// without naming a ROM. From https://github.com/A4091/a4091-software .
pub const A4091_ROM_FILE: &str = "a4091_cdfs.rom";

/// Locate the bundled A4091 ROM, searching the same places as
/// [`find_bundled_aros`] under an `a4091/` subdirectory.
pub fn find_bundled_a4091() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_A4091_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("a4091"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("a4091"));
                dirs.push(parent.join("share").join("copperline").join("a4091"));
            }
        }
    }
    dirs.push(PathBuf::from("assets").join("a4091"));

    dirs.into_iter()
        .map(|dir| dir.join(A4091_ROM_FILE))
        .find(|rom| rom.is_file())
}

/// Copperline's open A2091/A590 autoboot ROM.
pub const A2091_ROM_FILE: &str = "copperline-a2091.rom";

/// Locate the bundled A2091 ROM under the conventional package and source
/// locations, with an environment override for downstream testing.
pub fn find_bundled_a2091() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_A2091_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("a2091"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("a2091"));
                dirs.push(parent.join("share").join("copperline").join("a2091"));
            }
        }
    }
    dirs.push(PathBuf::from("assets").join("a2091"));

    dirs.into_iter()
        .map(|dir| dir.join(A2091_ROM_FILE))
        .find(|rom| rom.is_file())
}

/// Bundled open-source lide.device autoboot ROMs and CD-filesystem second
/// bank, used when a `[lide]` board is fitted without naming ROMs of its
/// own. From https://github.com/LIV2/lide.device . RIPPLE and RIDE share
/// `lide.rom`; AT-Bus 2008 needs its own `lide-atbus.rom` -- the two are
/// built from different linker scripts (`rom.ld` puts a 4-byte "LIV2"
/// header before the bootloader; `atbusrom.ld` starts the bootloader
/// straight at offset 0), matching the different `diag_vec` Copperline's
/// own `zorro::BoardSpec::lide` already uses per personality (0x0008 vs
/// 0x0001) -- not merely a byte-lane placement difference.
pub const LIDE_ROM_FILE: &str = "lide.rom";
/// AT-Bus 2008's own boot ROM build (bootloader at offset 0, no header).
pub const LIDE_ATBUS_ROM_FILE: &str = "lide-atbus.rom";
/// CD-filesystem second flash bank (RIPPLE/RIDE only).
pub const LIDE_CDFS_ROM_FILE: &str = "cdfs.rom";

/// A located set of bundled lide ROM files. `atbus`/`cdfs` are `None` when
/// only the files a particular install actually carries are present (e.g. a
/// lide/ directory shipping just `lide.rom` and `cdfs.rom`).
pub struct BundledLide {
    pub rom: PathBuf,
    pub atbus: Option<PathBuf>,
    pub cdfs: Option<PathBuf>,
}

/// Locate the bundled lide ROMs, searching the same places as
/// [`find_bundled_a4091`] under a `lide/` subdirectory.
pub fn find_bundled_lide() -> Option<BundledLide> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_LIDE_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("lide"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("lide"));
                dirs.push(parent.join("share").join("copperline").join("lide"));
            }
        }
    }
    dirs.push(PathBuf::from("assets").join("lide"));

    dirs.into_iter().find_map(|dir| {
        let rom = dir.join(LIDE_ROM_FILE);
        rom.is_file().then(|| {
            let atbus = dir.join(LIDE_ATBUS_ROM_FILE);
            let cdfs = dir.join(LIDE_CDFS_ROM_FILE);
            BundledLide {
                rom,
                atbus: atbus.is_file().then_some(atbus),
                cdfs: cdfs.is_file().then_some(cdfs),
            }
        })
    })
}
