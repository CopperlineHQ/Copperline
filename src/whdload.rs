// SPDX-License-Identifier: GPL-3.0-or-later

//! Direct WHDLoad boot: `--whdload game.lha` (or `[whdload] game = ...`)
//! boots straight into a WHDLoad-installed program with no Workbench disk
//! or hand-built hard drive.
//!
//! WHDLoad packages are `.lha` trees carrying the game files and a `.slave`
//! loader. Booting one needs a minimal AmigaOS hard-disk environment: the
//! WHDLoad binary itself, a `Startup-Sequence` that runs the slave, and --
//! for the many slaves that reboot the game under its original OS -- raw
//! Kickstart images (plus `.RTB` relocation tables) in `Devs:Kickstarts/`.
//! Other emulators assemble the same environment: Amiberry ships a
//! "WHDBooter" boot volume, FS-UAE Launcher synthesizes a temporary drive.
//!
//! Copperline stages two host directories and mounts them through the
//! services board (src/filesys.rs), so the guest reads and writes them
//! live:
//!
//! - `<library>/<game>/boot/` (volume `WHDBoot:`, boot priority 6): the
//!   WHDLoad binary (unpacked from the redistributable `WHDLoad_usr.lha`
//!   located by [`find_whdboot_assets`]), a generated `S/Startup-Sequence`,
//!   and `Devs/Kickstarts/` populated from the user's Kickstart images.
//!   Regenerated on every launch.
//! - `<library>/<game>/game/` (volume `WHDGame:`): the package, extracted
//!   once and then reused, so savegames and highscores the slave writes
//!   back persist across runs. Passing a directory instead of an archive
//!   mounts it in place.
//!
//! The machine is derived from the slave header (src/lha.rs reads the
//! archive, [`parse_slave`] the header): an A1200 profile -- the canonical
//! WHDLoad host, 68020 + AGA covers both `WHDLF_Req68020` and `WHDLF_ReqAGA`
//! -- with 8 MiB of fast RAM for `ws_ExpMem` and WHDLoad's Preload buffering.
//! Explicit user configuration (a `[machine]` profile, `rom`, or memory
//! sizes) always wins over the derivation.
//!
//! Kickstart images are identified by content, not filename: the CRC-16
//! WHDLoad itself uses (`lha::crc16`, "ANSI conform" per the autodocs)
//! computed after normalizing byte-swapped dumps, doubled 256 KiB images,
//! and Cloanto Amiga Forever `rom.key` encryption. A slave that declares
//! its Kickstart (ws_Version >= 16) is matched by declared size + CRC;
//! the well-known images are also staged under their canonical names for
//! older slaves that load `Devs:Kickstarts/` files at runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::{RawConfig, RawFilesysMount};
use crate::lha;

/// The unmodified WHDLoad distribution archive, as published on whdload.de.
/// The WHDLoad binary is unpacked from it at stage time; the archive is
/// fetched at packaging time (CI) or by `tools/fetch-whdload.sh`, never
/// committed to the repository.
pub const WHDLOAD_USR_ARCHIVE: &str = "WHDLoad_usr.lha";

/// The Soft-Kicker archive from Aminet (`util/boot/skick346.lha`), whose
/// `Kickstarts/*.RTB` relocation tables accompany the raw Kickstart images
/// WHDLoad loads into expansion memory.
pub const SKICK_ARCHIVE: &str = "skick346.lha";

/// Boot-volume name; the game volume follows it.
pub const BOOT_VOLUME: &str = "WHDBoot";
/// Game-volume name used in the generated Startup-Sequence.
pub const GAME_VOLUME: &str = "WHDGame";

/// DF0: enters the boot vote at priority 5; the staged boot volume must win.
const BOOT_PRIORITY: i8 = 6;

/// The located WHDLoad support assets.
pub struct WhdbootAssets {
    /// `WHDLoad_usr.lha`.
    pub whdload_archive: PathBuf,
    /// `skick346.lha`, when present (RTB staging is skipped without it).
    pub skick_archive: Option<PathBuf>,
}

/// Search the conventional install locations for the WHDLoad support
/// archives, mirroring `romsearch::find_bundled_aros`: an explicit
/// `COPPERLINE_WHDBOOT_DIR` override; locations relative to the running
/// executable (sibling `whdboot/`, macOS `.app` `Resources/whdboot/`,
/// Homebrew/Unix `../share/copperline/whdboot/`); and the source-tree
/// `assets/whdboot/` populated by `tools/fetch-whdload.sh`.
pub fn find_whdboot_assets() -> Option<WhdbootAssets> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(dir) = crate::envcfg::var("COPPERLINE_WHDBOOT_DIR") {
        dirs.push(PathBuf::from(dir));
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("whdboot"));
            if let Some(parent) = bin_dir.parent() {
                dirs.push(parent.join("Resources").join("whdboot"));
                dirs.push(parent.join("share").join("copperline").join("whdboot"));
            }
        }
    }

    // Where the launcher's Download button and tools/fetch-whdload.sh put
    // them. Tried before the development tree so a fetched copy wins over
    // a stale checked-out one.
    if let Some(dir) = crate::paths::whdload_support_dir() {
        dirs.push(dir);
    }

    dirs.push(PathBuf::from("assets").join("whdboot"));

    dirs.into_iter().find_map(|dir| {
        let whdload_archive = dir.join(WHDLOAD_USR_ARCHIVE);
        whdload_archive.is_file().then(|| {
            let skick = dir.join(SKICK_ARCHIVE);
            WhdbootAssets {
                whdload_archive,
                skick_archive: skick.is_file().then_some(skick),
            }
        })
    })
}

// ---------------------------------------------------------------------------
// Slave header parsing
// ---------------------------------------------------------------------------

/// `ws_Flags` bits (whdload.i).
pub mod flags {
    pub const DISK: u16 = 1 << 0;
    pub const NO_ERROR: u16 = 1 << 1;
    pub const EMUL_TRAP: u16 = 1 << 2;
    pub const NO_DIV_ZERO: u16 = 1 << 3;
    pub const REQ_68020: u16 = 1 << 4;
    pub const REQ_AGA: u16 = 1 << 5;
    pub const NO_KBD: u16 = 1 << 6;
    pub const EMUL_LINE_A: u16 = 1 << 7;
    pub const EMUL_TRAP_V: u16 = 1 << 8;
    pub const EMUL_CHK: u16 = 1 << 9;
    pub const EMUL_PRIV: u16 = 1 << 10;
    pub const EMUL_LINE_F: u16 = 1 << 11;
    pub const CLEAR_MEM: u16 = 1 << 12;
    pub const EXAMINE: u16 = 1 << 13;
    pub const EMUL_DIV_ZERO: u16 = 1 << 14;
    pub const EMUL_ILLEGAL: u16 = 1 << 15;
}

/// A Kickstart image a slave declares it needs (ws_Version >= 16): the
/// `Devs:Kickstarts/` file name, the image size, and the WHDLoad CRC-16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KickRequirement {
    pub name: String,
    pub size: u32,
    pub crc: u16,
}

/// The parsed WHDLoadSlave structure of a `.slave` file.
#[derive(Debug, Clone, Default)]
pub struct SlaveInfo {
    /// Required WHDLoad version (`ws_Version`).
    pub version: u16,
    /// `ws_Flags`; see [`flags`].
    pub flags: u16,
    /// `ws_BaseMemSize`: chip memory the installed program needs, in bytes.
    pub base_mem: u32,
    /// `ws_ExpMem` magnitude: expansion (fast) memory needed, in bytes.
    pub exp_mem: u32,
    /// Whether a non-zero `ws_ExpMem` was negative, i.e. optional.
    pub exp_mem_optional: bool,
    /// `ws_CurrentDir`: data subdirectory relative to the slave.
    pub current_dir: Option<String>,
    /// `ws_name` / `ws_copy` / `ws_info` splash strings.
    pub name: Option<String>,
    pub copyright: Option<String>,
    pub info: Option<String>,
    /// Kickstart images the slave accepts (empty when none declared). A
    /// single declared image parses to one entry; the WHDLoad 16.1 special
    /// mode (`ws_kickcrc` = $FFFF, a table of CRC/name pairs) to several.
    pub kicks: Vec<KickRequirement>,
}

impl SlaveInfo {
    pub fn requires_aga(&self) -> bool {
        self.flags & flags::REQ_AGA != 0
    }
    pub fn requires_68020(&self) -> bool {
        self.flags & flags::REQ_68020 != 0
    }
}

const HUNK_HEADER: u32 = 0x3F3;
const HUNK_CODE: u32 = 0x3E9;
/// `ws_Security`: `moveq #-1,d0` / `rts`.
const SLAVE_SECURITY: u32 = 0x70FF_4E75;
const SLAVE_ID: &[u8; 8] = b"WHDLOADS";

fn be32(data: &[u8], off: usize) -> Result<u32> {
    let bytes: [u8; 4] = data
        .get(off..off + 4)
        .and_then(|s| s.try_into().ok())
        .with_context(|| format!("slave file truncated at offset {off}"))?;
    Ok(u32::from_be_bytes(bytes))
}

fn be16(data: &[u8], off: usize) -> Result<u16> {
    let bytes: [u8; 2] = data
        .get(off..off + 2)
        .and_then(|s| s.try_into().ok())
        .with_context(|| format!("slave file truncated at offset {off}"))?;
    Ok(u16::from_be_bytes(bytes))
}

/// A NUL-terminated Latin-1 string at an RPTR (relative, 16-bit) offset from
/// the structure base; offset 0 means "absent".
fn rptr_string(code: &[u8], off: u16) -> Option<String> {
    if off == 0 {
        return None;
    }
    let start = off as usize;
    let bytes = code.get(start..)?;
    let end = bytes.iter().position(|&b| b == 0)?;
    Some(bytes[..end].iter().map(|&b| b as char).collect())
}

/// Parse the first code hunk out of an Amiga hunk executable. WHDLoad
/// requires a slave to be a single code hunk; the WHDLoadSlave structure
/// sits at its start.
fn first_code_hunk(bytes: &[u8]) -> Result<&[u8]> {
    if be32(bytes, 0)? != HUNK_HEADER {
        bail!("not an Amiga hunk executable (no HUNK_HEADER)");
    }
    if be32(bytes, 4)? != 0 {
        bail!("unexpected resident library names in slave hunk header");
    }
    let table_size = be32(bytes, 8)? as usize;
    let first = be32(bytes, 12)? as usize;
    let last = be32(bytes, 16)? as usize;
    if first != 0 || last + 1 - first != table_size {
        bail!("unexpected slave hunk table layout");
    }
    // Skip the per-hunk size table, then expect HUNK_CODE.
    let mut off = 20 + table_size * 4;
    if be32(bytes, off)? != HUNK_CODE {
        bail!("slave's first hunk is not HUNK_CODE");
    }
    off += 4;
    let words = be32(bytes, off)? as usize;
    off += 4;
    let len = words * 4;
    bytes
        .get(off..off + len)
        .with_context(|| "slave file shorter than its hunk length")
}

/// Parse a `.slave` file's WHDLoadSlave structure.
/// Whether a slave-declared Kickstart name is a plain `Devs:Kickstarts/`
/// file name. WHDLoad prepends that path itself, so the field's contract is
/// a bare name; anything carrying a path separator (or `.`/`..`) is not a
/// valid declaration, and honouring one would let a hostile package steer
/// the staging writes outside the generated boot volume.
fn safe_kick_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| matches!(c, '/' | '\\' | ':') || c.is_control())
}

pub fn parse_slave(bytes: &[u8]) -> Result<SlaveInfo> {
    let code = first_code_hunk(bytes)?;
    if be32(code, 0)? != SLAVE_SECURITY {
        bail!("missing WHDLoad security header (not a slave?)");
    }
    if code.get(4..12) != Some(SLAVE_ID.as_slice()) {
        bail!("missing WHDLOADS id (not a slave?)");
    }
    let version = be16(code, 12)?;
    let mut slave = SlaveInfo {
        version,
        flags: be16(code, 14)?,
        base_mem: be32(code, 16)?,
        current_dir: rptr_string(code, be16(code, 26)?),
        ..SlaveInfo::default()
    };
    if version >= 8 {
        let exp = be32(code, 32)? as i32;
        slave.exp_mem = exp.unsigned_abs();
        slave.exp_mem_optional = exp < 0;
    }
    if version >= 10 {
        slave.name = rptr_string(code, be16(code, 36)?);
        slave.copyright = rptr_string(code, be16(code, 38)?);
        slave.info = rptr_string(code, be16(code, 40)?);
    }
    if version >= 16 {
        let kickname = be16(code, 42)?;
        let kicksize = be32(code, 44)?;
        let kickcrc = be16(code, 48)?;
        if kickname != 0 && kicksize != 0 {
            if kickcrc == 0xFFFF {
                // WHDLoad 16.1 multi-image mode: a table of (CRC16, RPTR
                // name) pairs, terminated by CRC 0.
                let mut entry = kickname as usize;
                loop {
                    let crc = be16(code, entry)?;
                    if crc == 0 {
                        break;
                    }
                    let name_off = be16(code, entry + 2)?;
                    if let Some(name) = rptr_string(code, name_off).filter(|n| safe_kick_name(n)) {
                        slave.kicks.push(KickRequirement {
                            name,
                            size: kicksize,
                            crc,
                        });
                    } else {
                        log::warn!("whdload: slave declares an invalid Kickstart name; ignored");
                    }
                    entry += 4;
                }
            } else if let Some(name) = rptr_string(code, kickname).filter(|n| safe_kick_name(n)) {
                slave.kicks.push(KickRequirement {
                    name,
                    size: kicksize,
                    crc: kickcrc,
                });
            } else {
                log::warn!("whdload: slave declares an invalid Kickstart name; ignored");
            }
        }
    }
    Ok(slave)
}

// ---------------------------------------------------------------------------
// Kickstart image identification
// ---------------------------------------------------------------------------

/// The Kickstart images WHDLoad's documentation names for `Devs:Kickstarts/`
/// (dev package, `Docs/en/need.html`), identified by exact size and WHDLoad
/// CRC-16. CRCs verified against real dumps (Cloanto Amiga Forever Plus
/// canonical set; the 40068.A1200 value is also the community-known one).
pub const KNOWN_KICKSTARTS: [(u32, u16, &str); 5] = [
    (0x40000, 0xE9C6, "kick33180.A500"),
    (0x40000, 0xF9E3, "kick34005.A500"),
    (0x80000, 0x970C, "kick40063.A600"),
    (0x80000, 0x9FF5, "kick40068.A1200"),
    (0x80000, 0x75D3, "kick40068.A4000"),
];

/// The canonical `Devs:Kickstarts/` name for a normalized image, when known.
fn canonical_kick_name(size: u32, crc: u16) -> Option<&'static str> {
    KNOWN_KICKSTARTS
        .iter()
        .find(|&&(s, c, _)| s == size && c == crc)
        .map(|&(_, _, name)| name)
}

/// Amiga Forever's scrambled ROM container: an `AMIROMTYPE1` tag followed by
/// the image XORed with the repeating bytes of the adjacent `rom.key`.
const CLOANTO_TAG: &[u8] = b"AMIROMTYPE1";

/// A normalized (decoded, byte-order-restored, un-doubled) Kickstart image
/// read from the user's collection.
struct KickImage {
    source: PathBuf,
    data: Vec<u8>,
    crc: u16,
}

/// Read and normalize one candidate Kickstart file. Returns `None` (with a
/// log line where relevant) for files that cannot be a Kickstart image.
fn read_kick_candidate(path: &Path) -> Option<KickImage> {
    let len = std::fs::metadata(path).ok()?.len();
    // Plain 256/512 KiB images, or either with the 11-byte Cloanto tag.
    // Everything else cannot be a Kickstart image.
    let plausible = matches!(len, 0x40000 | 0x80000 | 0x4000B | 0x8000B);
    if !plausible {
        return None;
    }
    let mut data = std::fs::read(path).ok()?;
    if data.starts_with(CLOANTO_TAG) {
        let key_path = path.parent()?.join("rom.key");
        let Ok(key) = std::fs::read(&key_path) else {
            log::warn!(
                "whdload: {} is Cloanto-encrypted but {} is missing; skipped",
                path.display(),
                key_path.display()
            );
            return None;
        };
        if key.is_empty() {
            return None;
        }
        data = data[CLOANTO_TAG.len()..]
            .iter()
            .zip(key.iter().cycle())
            .map(|(&b, &k)| b ^ k)
            .collect();
    }
    // Byte-swapped EPROM-programmer dumps open $xx11 $F94E instead of the
    // big-endian $11xx $4EF9 ROM header (see memory.rs).
    if data.len() >= 4 && data[1] == 0x11 && data[2..4] == [0xF9, 0x4E] {
        for pair in data.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
    }
    // A 256 KiB part dumped through a 512 KiB window appears doubled.
    if data.len() == 0x80000 && data[..0x40000] == data[0x40000..] {
        data.truncate(0x40000);
    }
    if data.first() != Some(&0x11) {
        return None;
    }
    let crc = lha::crc16(&data);
    Some(KickImage {
        source: path.to_path_buf(),
        data,
        crc,
    })
}

/// Scan the given directories (non-recursively) for Kickstart images.
fn scan_kickstarts(dirs: &[PathBuf]) -> Vec<KickImage> {
    let mut images: Vec<KickImage> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        for path in paths {
            if let Some(image) = read_kick_candidate(&path) {
                let duplicate = images
                    .iter()
                    .any(|i| i.crc == image.crc && i.data.len() == image.data.len());
                if !duplicate {
                    images.push(image);
                }
            }
        }
    }
    images
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// Everything `prepare` needs beyond the game path itself; assembled from
/// `[whdload]` config keys by the caller.
#[derive(Default)]
pub struct Options {
    /// Game library root (extractions, saves, staged boot volumes).
    /// Defaults to `paths::config_dir()/whdload`.
    pub library: Option<PathBuf>,
    /// Directories scanned for Kickstart images, in order.
    pub kickstart_dirs: Vec<PathBuf>,
    /// Extra options appended to the generated WHDLoad command line.
    pub extra_args: Option<String>,
    /// Support archives; located via [`find_whdboot_assets`] when `None`.
    pub assets: Option<WhdbootAssets>,
}

/// A staged, mountable WHDLoad game.
#[derive(Debug)]
pub struct PreparedGame {
    /// Boot volume directory (`WHDBoot:`).
    pub boot_dir: PathBuf,
    /// Game volume directory (`WHDGame:`).
    pub game_dir: PathBuf,
    /// The chosen slave, relative to `game_dir`.
    pub slave_rel: PathBuf,
    /// Parsed slave header.
    pub slave: SlaveInfo,
    /// Staged machine ROM (`kick40068.A1200`), when the user's collection
    /// held one; the A1200 host boots the bundled AROS without it.
    pub machine_rom: Option<PathBuf>,
    /// `Devs:Kickstarts/` names staged into the boot volume.
    pub staged_kicks: Vec<String>,
}

/// The library directory a package unpacks into.
///
/// An `.lha` keeps the bare stem it has always used, so no existing
/// library entry moves -- the entry is where saves live, and relocating
/// one makes a person's savegames look lost.
///
/// Anything else keeps its extension. Somebody holding the same game as
/// both an `.lha` and a `.zip` has a duplicate, which is their business:
/// it lists twice and both play, with a set of saves each, rather than
/// the second one refusing to start because the first had taken the name.
fn library_entry_name(game: &Path) -> String {
    let file = game
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // By name, not by asking the filesystem: the caller has already
    // established this is a file, and `stem_of` drops only an extension
    // this recognises -- `S.W.I.V.lha` keeps its dots.
    let keeps_stem = matches!(
        crate::package::Kind::of_name(&file),
        Some(crate::package::Kind::Lha)
    );
    sanitize_game_name(match keeps_stem {
        true => crate::package::stem_of(&file),
        false => &file,
    })
}

/// Keep a game's library directory name readable and filesystem-safe.
fn sanitize_game_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '&' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = name.trim_matches(['_', '.'].as_slice());
    if trimmed.is_empty() {
        "game".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Find the `.slave` files under `dir`, relative to it.
fn find_slaves(dir: &Path) -> Result<Vec<PathBuf>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("reading directory {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            // file_type() does not follow symlinks: a link to an ancestor
            // must not recurse forever, so links are skipped outright (the
            // slave search only decides what to boot; the mounted volume
            // itself still serves whatever the package contains).
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                walk(root, &path, out)?;
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("slave"))
            {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                // `__MACOSX/._Foo.Slave` ends in `.Slave` and sorts before
                // the real one. Booting it starts nothing.
                if !crate::package::is_rubbish(&rel) {
                    out.push(rel);
                }
            }
        }
        Ok(())
    }
    let mut slaves = Vec::new();
    walk(dir, dir, &mut slaves)?;
    // Deterministic preference: shallowest, then lexicographic.
    slaves.sort_by_key(|p| (p.components().count(), p.to_string_lossy().to_lowercase()));
    Ok(slaves)
}

/// An AmigaDOS path (`/`-separated) for a relative host path.
fn amiga_path(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The generated `S/Startup-Sequence`. `CD` and `FailAt` are internal shell
/// commands from Kickstart 2.0 on, so the boot volume needs no other
/// commands than WHDLoad itself.
fn startup_sequence(slave_rel: &Path, extra_args: Option<&str>) -> String {
    let mut lines = vec!["FailAt 21".to_string()];
    let slave_name = slave_rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match slave_rel.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(subdir) => lines.push(format!("CD \"{GAME_VOLUME}:{}\"", amiga_path(subdir))),
        None => lines.push(format!("CD {GAME_VOLUME}:")),
    }
    let mut run = format!("C:WHDLoad \"{slave_name}\" Preload SplashDelay=0");
    if let Some(args) = extra_args {
        let args = args.trim();
        if !args.is_empty() {
            run.push(' ');
            run.push_str(args);
        }
    }
    lines.push(run);
    lines.push(String::new());
    lines.join("\n")
}

/// Read the `[whdload]` configuration: the configured game (if any) and the
/// staging options. The Kickstart search list is the configured
/// `kickstarts` directory alone when set; otherwise the directory of an
/// explicitly configured `rom` and `<library>/Kickstarts` (the directory
/// holding the support archives is always tried last, inside [`prepare`]).
pub fn game_and_options(raw: &RawConfig) -> (Option<PathBuf>, Options) {
    let library = raw.whdload.library.as_ref().map(PathBuf::from);
    let mut kickstart_dirs = Vec::new();
    match &raw.whdload.kickstarts {
        Some(dir) => kickstart_dirs.push(PathBuf::from(dir)),
        None => {
            if let Some(rom) = &raw.rom {
                if let Some(dir) = Path::new(rom).parent() {
                    if !dir.as_os_str().is_empty() {
                        kickstart_dirs.push(dir.to_path_buf());
                    }
                }
            }
            let lib = library.clone().or_else(crate::paths::whdload_save_dir);
            if let Some(lib) = lib {
                kickstart_dirs.push(lib.join("Kickstarts"));
            }
        }
    }
    let game = raw.whdload.game.as_ref().map(PathBuf::from);
    (
        game,
        Options {
            library,
            kickstart_dirs,
            extra_args: raw.whdload.args.clone(),
            assets: configured_assets(raw),
        },
    )
}

/// The support archives the configuration names, if it names either.
///
/// `None` leaves [`prepare`] to search, which is what an installation that
/// has never been told anything wants. Naming one and not the other is
/// meant to work: the half that was named is used, and the half that was
/// not still comes from the search.
fn configured_assets(raw: &RawConfig) -> Option<WhdbootAssets> {
    let whd = raw.whdload.whd_package.as_ref().map(PathBuf::from);
    let skick = raw.whdload.skick_package.as_ref().map(PathBuf::from);
    if whd.is_none() && skick.is_none() {
        return None;
    }
    let found = find_whdboot_assets();
    Some(WhdbootAssets {
        whdload_archive: whd.or_else(|| found.as_ref().map(|f| f.whdload_archive.clone()))?,
        skick_archive: skick.or_else(|| found.and_then(|f| f.skick_archive)),
    })
}

/// Stage `game` -- an `.lha` or `.zip` archive, or a directory holding a
/// `.slave` -- and return the mountable result.
pub fn prepare(game: &Path, opts: &Options) -> Result<PreparedGame> {
    if !game.exists() {
        bail!("WHDLoad game {} does not exist", game.display());
    }

    let assets = match &opts.assets {
        Some(assets) => WhdbootAssets {
            whdload_archive: assets.whdload_archive.clone(),
            skick_archive: assets.skick_archive.clone(),
        },
        None => match find_whdboot_assets() {
            Some(assets) => assets,
            None => {
                // The status bar gets the one sentence that says what is
                // wrong; what to do about it goes to the log, where there
                // is room for it.
                log::error!(
                    "whdload: {WHDLOAD_USR_ARCHIVE} was not found. Press Download on \
                     the WHDLoad page, run tools/fetch-whdload.sh, or point \
                     COPPERLINE_WHDBOOT_DIR at a directory holding it."
                );
                bail!("Missing WHDLoad support archive");
            }
        },
    };
    // A path that was configured and then moved or deleted fails the same
    // way as one that was never there, and says the same thing.
    if !assets.whdload_archive.is_file() {
        log::error!(
            "whdload: {} is not there. Set [whdload] whd_package, or clear it and \
             press Download on the WHDLoad page.",
            assets.whdload_archive.display()
        );
        bail!("Missing WHDLoad support archive");
    }
    let assets = WhdbootAssets {
        skick_archive: assets.skick_archive.filter(|skick| {
            skick.is_file() || {
                log::warn!(
                    "whdload: missing SKick support archive at {}; Kickstart images \
                     will be staged without .RTB relocation tables",
                    skick.display()
                );
                false
            }
        }),
        ..assets
    };

    let library = match &opts.library {
        Some(dir) => dir.clone(),
        None => crate::paths::whdload_save_dir().context(
            "no per-user directory available for the WHDLoad game library; \
                 set [whdload] library in the configuration",
        )?,
    };

    let game_home = library.join(library_entry_name(game));

    // --- Game volume -------------------------------------------------------
    let game_dir = if game.is_dir() {
        game.to_path_buf()
    } else {
        // The extraction is a cache keyed by the sanitized archive stem, so
        // two guards make reuse trustworthy: a marker recording the source
        // archive's file name (written only after a completed extraction,
        // and sitting outside the mounted game/ tree) catches both a
        // half-finished extraction and two different archives whose stems
        // sanitize to the same library entry; and the extraction lands in a
        // temporary sibling promoted by rename, so game/ never holds a
        // partial tree.
        let dest = game_home.join("game");
        let marker = game_home.join(".source");
        let source_name = game
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let recorded = std::fs::read_to_string(&marker)
            .ok()
            .map(|s| s.trim().to_string());
        if dest.is_dir() && recorded.as_deref() == Some(source_name.as_str()) {
            log::info!(
                "whdload: reusing extracted game in {} (saves persist there)",
                dest.display()
            );
        } else if dest.is_dir() && recorded.is_some() {
            bail!(
                "game library entry {} was extracted from \"{}\", but this game is \
                 \"{source_name}\"; rename one archive or use a different [whdload] library",
                game_home.display(),
                recorded.unwrap_or_default(),
            );
        } else {
            if dest.exists() {
                // A directory without its marker is a leftover from an
                // interrupted extraction; nothing in it can be trusted.
                std::fs::remove_dir_all(&dest)
                    .with_context(|| format!("clearing partial extraction {}", dest.display()))?;
            }
            let staging = game_home.join("game.extracting");
            if staging.exists() {
                std::fs::remove_dir_all(&staging)
                    .with_context(|| format!("clearing {}", staging.display()))?;
            }
            std::fs::create_dir_all(&staging)
                .with_context(|| format!("creating {}", staging.display()))?;
            let files = crate::package::extract_to_dir(game, &staging)
                .with_context(|| format!("extracting {}", game.display()))?;
            std::fs::rename(&staging, &dest)
                .with_context(|| format!("promoting extraction into {}", dest.display()))?;
            std::fs::write(&marker, format!("{source_name}\n"))
                .with_context(|| format!("writing {}", marker.display()))?;
            log::info!(
                "whdload: extracted {} files from {} into {}",
                files,
                game.display(),
                dest.display()
            );
        }
        dest
    };

    let slaves = find_slaves(&game_dir)?;
    let slave_rel = match slaves.as_slice() {
        [] => bail!(
            "{} contains no .slave file; not a WHDLoad package?",
            game.display()
        ),
        [one] => one.clone(),
        [first, ..] => {
            log::info!(
                "whdload: {} slaves in the package, using {}",
                slaves.len(),
                first.display()
            );
            first.clone()
        }
    };
    let slave_bytes = std::fs::read(game_dir.join(&slave_rel))
        .with_context(|| format!("reading slave {}", slave_rel.display()))?;
    let slave = parse_slave(&slave_bytes)
        .with_context(|| format!("parsing slave {}", slave_rel.display()))?;
    if slave.exp_mem > 8 * 1024 * 1024 && !slave.exp_mem_optional {
        // The derived A1200 host tops out at the Zorro II 8 MiB; the guest
        // WHDLoad will refuse with its own requester if this really is
        // short, so warn with the fix rather than failing the launch.
        log::warn!(
            "whdload: slave declares {} MiB of expansion memory; the derived A1200 \
             fits 8 MiB fast RAM -- configure a bigger machine ([machine]/[memory] \
             or --model/--fast) if the program refuses to start",
            slave.exp_mem >> 20
        );
    }

    // --- Boot volume -------------------------------------------------------
    let boot_dir = game_home.join("boot");
    if boot_dir.exists() {
        std::fs::remove_dir_all(&boot_dir)
            .with_context(|| format!("clearing {}", boot_dir.display()))?;
    }
    std::fs::create_dir_all(boot_dir.join("C"))?;
    std::fs::create_dir_all(boot_dir.join("S"))?;
    let kick_dir = boot_dir.join("Devs").join("Kickstarts");
    std::fs::create_dir_all(&kick_dir)?;

    let whdload_bin = lha::read_member(&assets.whdload_archive, Path::new("WHDLoad/C/WHDLoad"))
        .with_context(|| {
            format!(
                "unpacking the WHDLoad binary from {}",
                assets.whdload_archive.display()
            )
        })?;
    std::fs::write(boot_dir.join("C").join("WHDLoad"), whdload_bin)?;
    std::fs::write(
        boot_dir.join("S").join("Startup-Sequence"),
        startup_sequence(&slave_rel, opts.extra_args.as_deref()),
    )?;

    // --- Kickstart images --------------------------------------------------
    let mut kickstart_dirs = opts.kickstart_dirs.clone();
    if let Some(dir) = assets.whdload_archive.parent() {
        // Users may drop Kickstart images next to the support archives.
        kickstart_dirs.push(dir.join("Kickstarts"));
    }
    let images = scan_kickstarts(&kickstart_dirs);
    // name -> image data to stage. BTreeMap for deterministic staging order.
    let mut stage: BTreeMap<String, &KickImage> = BTreeMap::new();
    for image in &images {
        if let Some(name) = canonical_kick_name(image.data.len() as u32, image.crc) {
            stage.entry(name.to_string()).or_insert(image);
        }
    }
    // Slave-declared images are matched strictly by their declared size and
    // CRC -- the values WHDLoad itself verifies at load time -- and staged
    // under the declared name, overriding a canonical entry that happens to
    // share it (the declaration is authoritative for that name). A canonical
    // image under the same name does NOT satisfy the requirement: with the
    // wrong content WHDLoad would only fail later, in the guest, instead of
    // here with the precise ask.
    let mut requirement_met = slave.kicks.is_empty();
    for req in &slave.kicks {
        if let Some(image) = images
            .iter()
            .find(|i| i.data.len() as u32 == req.size && i.crc == req.crc)
        {
            stage.insert(req.name.clone(), image);
            requirement_met = true;
        }
    }

    if !requirement_met {
        let wanted = slave
            .kicks
            .iter()
            .map(|req| {
                format!(
                    "{} ({} KiB, CRC16 ${:04X})",
                    req.name,
                    req.size / 1024,
                    req.crc
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let searched = if kickstart_dirs.is_empty() {
            "no Kickstart directories are configured ([whdload] kickstarts)".to_string()
        } else {
            format!(
                "searched: {}",
                kickstart_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        bail!(
            "this slave needs a Kickstart image it can load from Devs:Kickstarts/ \
             ({wanted}); {searched}. Copperline identifies images by content, so any \
             file name works"
        );
    }

    let mut staged_kicks = Vec::new();
    for (name, image) in &stage {
        std::fs::write(kick_dir.join(name), &image.data)
            .with_context(|| format!("staging {name}"))?;
        log::info!(
            "whdload: staged Devs:Kickstarts/{name} from {}",
            image.source.display()
        );
        if let Some(skick) = &assets.skick_archive {
            let member = PathBuf::from("Kickstarts").join(format!("{name}.RTB"));
            match lha::read_member(skick, &member) {
                Ok(rtb) => {
                    std::fs::write(kick_dir.join(format!("{name}.RTB")), rtb)?;
                }
                Err(_) => log::warn!(
                    "whdload: no {name}.RTB in {}; OCS/ECS slaves needing it will abort",
                    skick.display()
                ),
            }
        }
        staged_kicks.push(name.clone());
    }
    if assets.skick_archive.is_none() && !staged_kicks.is_empty() {
        log::warn!(
            "whdload: missing SKick support archive ({SKICK_ARCHIVE}); Kickstart \
             images staged without .RTB relocation tables"
        );
    }

    // The A1200 host machine boots best from its own Kickstart; a staged
    // kick40068.A1200 doubles as the machine ROM (already decoded and
    // byte-order-normalized).
    let machine_rom = stage
        .contains_key("kick40068.A1200")
        .then(|| kick_dir.join("kick40068.A1200"));

    if let Some(name) = &slave.name {
        log::info!("whdload: {}", name);
    }

    Ok(PreparedGame {
        boot_dir,
        game_dir,
        slave_rel,
        slave,
        machine_rom,
        staged_kicks,
    })
}

/// Record the game in the raw configuration's own `[whdload]` section, so a
/// configuration retained for the session (and later edited or saved from
/// the launcher) carries the user's intent -- unlike [`apply_to_raw`]'s
/// derived machine and mounts, which belong only to a throwaway clone.
pub fn remember_game(raw: &mut RawConfig, game: &Path) {
    raw.whdload.game = Some(game.to_string_lossy().into_owned());
}

/// Apply a prepared game to the raw configuration: derive the machine where
/// the user has not chosen one, and mount the two staged volumes. Explicit
/// configuration (a `[machine]` profile, `rom`, `[memory]` sizes, CLI
/// overrides, which land in the raw config before this) always wins.
pub fn apply_to_raw(raw: &mut RawConfig, prepared: &PreparedGame) {
    if raw.machine.profile.is_none() {
        // The canonical WHDLoad host: 68020 + AGA satisfies every slave
        // requirement flag, and OCS/ECS programs run under the slave's own
        // hardware bending, exactly as on a real A1200.
        raw.machine.profile = Some("A1200".to_string());
    }
    if raw.memory.fast.is_none() {
        // Preload wants room for the whole game on top of ws_ExpMem; the
        // full Zorro II 8 MiB is the canonical WHDLoad recommendation.
        raw.memory.fast = Some("8M".to_string());
    }
    if raw.rom.is_none() {
        match &prepared.machine_rom {
            Some(rom) => raw.rom = Some(rom.to_string_lossy().into_owned()),
            None => log::warn!(
                "whdload: no Kickstart 3.1 (40.068 A1200) image found; booting the \
                 bundled AROS ROM instead -- many WHDLoad programs need the real \
                 Kickstart, so expect reduced compatibility"
            ),
        }
    }
    raw.filesys.push(RawFilesysMount {
        path: prepared.boot_dir.to_string_lossy().into_owned(),
        volume: Some(BOOT_VOLUME.to_string()),
        bootpri: Some(BOOT_PRIORITY),
        readonly: None,
    });
    raw.filesys.push(RawFilesysMount {
        path: prepared.game_dir.to_string_lossy().into_owned(),
        volume: Some(GAME_VOLUME.to_string()),
        bootpri: None,
        readonly: None,
    });
}

#[cfg(test)]
mod tests {
    /// Somebody's own WHDLoad and Soft-Kicker archives are used, not
    /// overridden by the copies Copperline knows how to fetch.
    ///
    /// The pinned digests are a convenience for getting started, not a
    /// requirement: a person testing a new WHDLoad release, or one who
    /// keeps their own build, points the configuration at it and that is
    /// what gets staged.
    #[test]
    fn each_package_format_unpacks_somewhere_of_its_own() {
        // Holding the same game as both an .lha and a .zip is a
        // duplicate, which is the person's business: both play, with a set
        // of saves each. The .lha keeps the bare-stem entry it has always
        // used, so no existing library moves -- that is where saves live.
        assert_eq!(
            library_entry_name(Path::new("/g/GoldenAxe_v1.lha")),
            "GoldenAxe_v1"
        );
        assert_eq!(
            library_entry_name(Path::new("/g/GoldenAxe_v1.zip")),
            "GoldenAxe_v1.zip"
        );
        assert_ne!(
            library_entry_name(Path::new("/g/GoldenAxe_v1.lha")),
            library_entry_name(Path::new("/g/GoldenAxe_v1.zip"))
        );
        // Only a recognised extension comes off, so a title with dots in
        // it keeps them.
        assert_eq!(library_entry_name(Path::new("/g/S.W.I.V.lha")), "S.W.I.V");
    }

    #[test]
    fn a_hand_set_support_archive_is_the_one_used() {
        use crate::config::RawConfig;
        let mut raw = RawConfig::default();
        assert!(
            super::configured_assets(&raw).is_none(),
            "an unconfigured build should search rather than insist"
        );

        raw.whdload.whd_package = Some("/my/WHDLoad_usr.lha".to_string());
        raw.whdload.skick_package = Some("/my/skick.lha".to_string());
        let assets = super::configured_assets(&raw).expect("both were named");
        assert_eq!(assets.whdload_archive, Path::new("/my/WHDLoad_usr.lha"));
        assert_eq!(
            assets.skick_archive.as_deref(),
            Some(Path::new("/my/skick.lha"))
        );

        // Naming one and not the other works: the half that was named is
        // used and the other still comes from the search, so somebody
        // testing a new WHDLoad keeps the Soft-Kicker they already had.
        raw.whdload.skick_package = None;
        let assets = super::configured_assets(&raw).expect("one was named");
        assert_eq!(assets.whdload_archive, Path::new("/my/WHDLoad_usr.lha"));
    }

    use super::*;
    use crate::lha::tests::{build_lha, temp_dir};

    /// Synthesize a minimal single-code-hunk slave, mirroring
    /// tests/assets/whdload/testgame.asm.
    pub(crate) fn build_slave(
        version: u16,
        flags: u16,
        base_mem: u32,
        exp_mem: i32,
        kick: Option<(&str, u32, u16)>,
    ) -> Vec<u8> {
        let mut code = Vec::new();
        code.extend_from_slice(&SLAVE_SECURITY.to_be_bytes());
        code.extend_from_slice(SLAVE_ID);
        code.extend_from_slice(&version.to_be_bytes());
        code.extend_from_slice(&flags.to_be_bytes());
        code.extend_from_slice(&base_mem.to_be_bytes());
        code.extend_from_slice(&0u32.to_be_bytes()); // ws_ExecInstall
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_GameLoader
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_CurrentDir
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_DontCache
        code.push(0); // ws_keydebug
        code.push(0x45); // ws_keyexit
        code.extend_from_slice(&exp_mem.to_be_bytes());
        // ws_name/copy/info, ws_kickname, ws_kicksize, ws_kickcrc, ws_config.
        // The strings sit directly after the 52-byte structure.
        let name_bytes = b"Test Game\0";
        let name_off = 52u16;
        let kick_name_off = name_off + name_bytes.len() as u16;
        code.extend_from_slice(&name_off.to_be_bytes());
        code.extend_from_slice(&0u16.to_be_bytes());
        code.extend_from_slice(&0u16.to_be_bytes());
        let (kick_off, kick_size, kick_crc) = match kick {
            Some((_, size, crc)) => (kick_name_off, size, crc),
            None => (0u16, 0, 0),
        };
        code.extend_from_slice(&kick_off.to_be_bytes());
        code.extend_from_slice(&kick_size.to_be_bytes());
        code.extend_from_slice(&kick_crc.to_be_bytes());
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_config
        assert_eq!(code.len(), 52);
        code.extend_from_slice(name_bytes);
        if let Some((name, _, _)) = kick {
            assert_eq!(code.len(), kick_name_off as usize);
            code.extend_from_slice(name.as_bytes());
            code.push(0);
        }
        wrap_code_hunk(code)
    }

    /// Wrap raw code bytes into a single-code-hunk Amiga executable.
    fn wrap_code_hunk(mut code: Vec<u8>) -> Vec<u8> {
        while !code.len().is_multiple_of(4) {
            code.push(0);
        }
        let words = (code.len() / 4) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&HUNK_HEADER.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes()); // table size
        out.extend_from_slice(&0u32.to_be_bytes()); // first
        out.extend_from_slice(&0u32.to_be_bytes()); // last
        out.extend_from_slice(&words.to_be_bytes()); // size table
        out.extend_from_slice(&HUNK_CODE.to_be_bytes());
        out.extend_from_slice(&words.to_be_bytes());
        out.extend_from_slice(&code);
        out
    }

    #[test]
    fn parses_the_assembled_test_slave_fixture() {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets/whdload/Test.Slave"),
        );
        let Ok(bytes) = bytes else {
            return; // fixture not yet present in this tree state
        };
        let slave = parse_slave(&bytes).unwrap();
        assert_eq!(slave.version, 10);
        assert_eq!(slave.base_mem, 0x80000);
        assert_eq!(slave.name.as_deref(), Some("Copperline WHDLoad probe"));
        assert!(slave.kicks.is_empty());
    }

    #[test]
    fn parses_synthesized_slave_headers() {
        let bytes = build_slave(
            17,
            flags::REQ_AGA | flags::REQ_68020,
            0x100000,
            -(0x200000i32),
            Some(("kick40068.A1200", 0x80000, 0x9FF5)),
        );
        let slave = parse_slave(&bytes).unwrap();
        assert_eq!(slave.version, 17);
        assert!(slave.requires_aga());
        assert!(slave.requires_68020());
        assert_eq!(slave.base_mem, 0x100000);
        assert_eq!(slave.exp_mem, 0x200000);
        assert!(slave.exp_mem_optional);
        assert_eq!(
            slave.kicks,
            vec![KickRequirement {
                name: "kick40068.A1200".to_string(),
                size: 0x80000,
                crc: 0x9FF5,
            }]
        );
        assert_eq!(slave.name.as_deref(), Some("Test Game"));
    }

    /// The WHDLoad 16.1 multi-image mode: `ws_kickcrc` = $FFFF and
    /// `ws_kickname` pointing at (CRC16, name-RPTR) pairs, CRC 0 terminated.
    #[test]
    fn parses_multi_kick_tables() {
        let mut code = Vec::new();
        code.extend_from_slice(&SLAVE_SECURITY.to_be_bytes());
        code.extend_from_slice(SLAVE_ID);
        code.extend_from_slice(&17u16.to_be_bytes()); // ws_Version
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_Flags
        code.extend_from_slice(&0x80000u32.to_be_bytes()); // ws_BaseMemSize
        code.extend_from_slice(&0u32.to_be_bytes()); // ws_ExecInstall
        code.extend_from_slice(&[0; 6]); // GameLoader/CurrentDir/DontCache
        code.push(0); // ws_keydebug
        code.push(0); // ws_keyexit
        code.extend_from_slice(&0u32.to_be_bytes()); // ws_ExpMem
        code.extend_from_slice(&[0; 6]); // ws_name/copy/info
        let table_off = 52u16;
        let name1_off = table_off + 3 * 4; // three 4-byte table rows
        let name1 = b"kick34005.A500\0";
        let name2_off = name1_off + name1.len() as u16;
        let name2 = b"kick33180.A500\0";
        code.extend_from_slice(&table_off.to_be_bytes()); // ws_kickname
        code.extend_from_slice(&0x40000u32.to_be_bytes()); // ws_kicksize
        code.extend_from_slice(&0xFFFFu16.to_be_bytes()); // ws_kickcrc
        code.extend_from_slice(&0u16.to_be_bytes()); // ws_config
        assert_eq!(code.len(), table_off as usize);
        for (crc, off) in [(0xF9E3u16, name1_off), (0xE9C6, name2_off), (0, 0)] {
            code.extend_from_slice(&crc.to_be_bytes());
            code.extend_from_slice(&off.to_be_bytes());
        }
        code.extend_from_slice(name1);
        code.extend_from_slice(name2);

        let slave = parse_slave(&wrap_code_hunk(code)).unwrap();
        assert_eq!(
            slave.kicks,
            vec![
                KickRequirement {
                    name: "kick34005.A500".to_string(),
                    size: 0x40000,
                    crc: 0xF9E3,
                },
                KickRequirement {
                    name: "kick33180.A500".to_string(),
                    size: 0x40000,
                    crc: 0xE9C6,
                },
            ]
        );
    }

    #[test]
    fn rejects_non_slave_executables() {
        assert!(parse_slave(b"not a hunk file").is_err());
        // A valid hunk wrapper whose code does not open with the security
        // longword.
        let mut bytes = build_slave(10, 0, 0x80000, 0, None);
        // Corrupt the security longword (code starts after the 32-byte
        // header).
        bytes[32] = 0;
        assert!(parse_slave(&bytes).is_err());
    }

    fn fake_kick(size: usize, seed: u8) -> Vec<u8> {
        let mut data = vec![0u8; size];
        data[0] = 0x11;
        data[1] = if size == 0x40000 { 0x11 } else { 0x14 };
        data[2] = 0x4E;
        data[3] = 0xF9;
        let mut state = seed;
        for byte in data.iter_mut().skip(4) {
            state = state.wrapping_mul(29).wrapping_add(17);
            *byte = state;
        }
        data
    }

    #[test]
    fn kick_normalization_handles_all_dump_forms() {
        let dir = temp_dir("kicks");
        let plain = fake_kick(0x40000, 3);
        let crc = lha::crc16(&plain);

        std::fs::write(dir.join("plain.rom"), &plain).unwrap();

        let mut doubled = plain.clone();
        doubled.extend_from_slice(&plain);
        std::fs::write(dir.join("doubled.rom"), &doubled).unwrap();

        let mut swapped = plain.clone();
        for pair in swapped.chunks_exact_mut(2) {
            pair.swap(0, 1);
        }
        std::fs::write(dir.join("swapped.bin"), &swapped).unwrap();

        let key = b"secret-key";
        let mut cloanto = CLOANTO_TAG.to_vec();
        cloanto.extend(plain.iter().zip(key.iter().cycle()).map(|(&b, &k)| b ^ k));
        std::fs::write(dir.join("cloanto.rom"), &cloanto).unwrap();
        std::fs::write(dir.join("rom.key"), key).unwrap();

        std::fs::write(dir.join("not-a-rom.adf"), vec![0u8; 901120]).unwrap();

        for name in ["plain.rom", "doubled.rom", "swapped.bin", "cloanto.rom"] {
            let image = read_kick_candidate(&dir.join(name))
                .unwrap_or_else(|| panic!("{name} should normalize"));
            assert_eq!(image.data, plain, "{name}");
            assert_eq!(image.crc, crc, "{name}");
        }
        assert!(read_kick_candidate(&dir.join("not-a-rom.adf")).is_none());

        // The scan deduplicates the four equal images.
        let images = scan_kickstarts(std::slice::from_ref(&dir));
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].crc, crc);
    }

    #[test]
    fn canonical_table_identifies_the_documented_images() {
        assert_eq!(
            canonical_kick_name(0x80000, 0x9FF5),
            Some("kick40068.A1200")
        );
        assert_eq!(canonical_kick_name(0x40000, 0xF9E3), Some("kick34005.A500"));
        assert_eq!(canonical_kick_name(0x40000, 0x1234), None);
    }

    #[test]
    fn startup_sequence_quotes_and_cds_into_the_slave_directory() {
        let script = startup_sequence(Path::new("Aladdin AGA/Aladdin.Slave"), None);
        assert_eq!(
            script,
            "FailAt 21\nCD \"WHDGame:Aladdin AGA\"\nC:WHDLoad \"Aladdin.Slave\" Preload SplashDelay=0\n"
        );
        let script = startup_sequence(Path::new("Test.Slave"), Some("ButtonWait"));
        assert_eq!(
            script,
            "FailAt 21\nCD WHDGame:\nC:WHDLoad \"Test.Slave\" Preload SplashDelay=0 ButtonWait\n"
        );
    }

    /// Full staging walk against synthetic assets: archive extraction, slave
    /// choice, WHDLoad unpack, Startup-Sequence, declared-kick matching by
    /// content, RTB staging, machine derivation.
    #[test]
    fn prepare_stages_boot_and_game_volumes() {
        let dir = temp_dir("prepare");

        let kick = fake_kick(0x80000, 7);
        let kick_crc = lha::crc16(&kick);
        let kick_dir = dir.join("roms");
        std::fs::create_dir_all(&kick_dir).unwrap();
        std::fs::write(kick_dir.join("mystery.rom"), &kick).unwrap();

        let slave = build_slave(
            17,
            0,
            0x80000,
            0,
            Some(("kick99999.A500", 0x80000, kick_crc)),
        );
        let game_lha = dir.join("Test Game.lha");
        std::fs::write(
            &game_lha,
            build_lha(&[
                ("TestGame.info", b"icon"),
                ("TestGame/Test.Slave", &slave),
                ("TestGame/data/file", b"payload"),
            ]),
        )
        .unwrap();

        let assets_dir = dir.join("whdboot");
        std::fs::create_dir_all(&assets_dir).unwrap();
        std::fs::write(
            assets_dir.join(WHDLOAD_USR_ARCHIVE),
            build_lha(&[("WHDLoad/C/WHDLoad", b"whdload-binary")]),
        )
        .unwrap();
        std::fs::write(
            assets_dir.join(SKICK_ARCHIVE),
            build_lha(&[("Kickstarts/kick99999.A500.RTB", b"rtb-data")]),
        )
        .unwrap();

        let opts = Options {
            library: Some(dir.join("library")),
            kickstart_dirs: vec![kick_dir],
            extra_args: None,
            assets: Some(WhdbootAssets {
                whdload_archive: assets_dir.join(WHDLOAD_USR_ARCHIVE),
                skick_archive: Some(assets_dir.join(SKICK_ARCHIVE)),
            }),
        };
        let prepared = prepare(&game_lha, &opts).unwrap();

        assert_eq!(prepared.slave_rel, PathBuf::from("TestGame/Test.Slave"));
        assert_eq!(
            std::fs::read(prepared.boot_dir.join("C/WHDLoad")).unwrap(),
            b"whdload-binary"
        );
        let script = std::fs::read_to_string(prepared.boot_dir.join("S/Startup-Sequence")).unwrap();
        assert!(script.contains("CD \"WHDGame:TestGame\""));
        assert!(script.contains("C:WHDLoad \"Test.Slave\" Preload"));
        assert_eq!(
            std::fs::read(prepared.boot_dir.join("Devs/Kickstarts/kick99999.A500")).unwrap(),
            kick
        );
        assert_eq!(
            std::fs::read(prepared.boot_dir.join("Devs/Kickstarts/kick99999.A500.RTB")).unwrap(),
            b"rtb-data"
        );
        assert!(prepared.machine_rom.is_none()); // synthetic CRC is not 40068.A1200
        assert_eq!(
            std::fs::read(prepared.game_dir.join("TestGame/data/file")).unwrap(),
            b"payload"
        );

        // Second run reuses the extraction (saves persist).
        std::fs::write(prepared.game_dir.join("TestGame/data/savegame"), b"save").unwrap();
        let again = prepare(&game_lha, &opts).unwrap();
        assert_eq!(
            std::fs::read(again.game_dir.join("TestGame/data/savegame")).unwrap(),
            b"save"
        );
    }

    #[test]
    fn prepare_fails_clearly_when_a_declared_kick_is_missing() {
        let dir = temp_dir("missing-kick");
        let slave = build_slave(17, 0, 0x80000, 0, Some(("kick34005.A500", 0x40000, 0xF9E3)));
        let game_lha = dir.join("game.lha");
        std::fs::write(&game_lha, build_lha(&[("Game/Game.Slave", &slave)])).unwrap();
        let assets_dir = dir.join("whdboot");
        std::fs::create_dir_all(&assets_dir).unwrap();
        std::fs::write(
            assets_dir.join(WHDLOAD_USR_ARCHIVE),
            build_lha(&[("WHDLoad/C/WHDLoad", b"bin")]),
        )
        .unwrap();

        let opts = Options {
            library: Some(dir.join("library")),
            kickstart_dirs: vec![],
            extra_args: None,
            assets: Some(WhdbootAssets {
                whdload_archive: assets_dir.join(WHDLOAD_USR_ARCHIVE),
                skick_archive: None,
            }),
        };
        let err = format!("{:#}", prepare(&game_lha, &opts).unwrap_err());
        assert!(err.contains("kick34005.A500"), "{err}");
        assert!(err.contains("F9E3"), "{err}");
    }

    /// Minimal support-archive fixtures for `prepare` tests.
    fn fixture_assets(dir: &Path) -> WhdbootAssets {
        let assets_dir = dir.join("whdboot");
        std::fs::create_dir_all(&assets_dir).unwrap();
        std::fs::write(
            assets_dir.join(WHDLOAD_USR_ARCHIVE),
            build_lha(&[("WHDLoad/C/WHDLoad", b"bin")]),
        )
        .unwrap();
        WhdbootAssets {
            whdload_archive: assets_dir.join(WHDLOAD_USR_ARCHIVE),
            skick_archive: None,
        }
    }

    /// Rewrite the last two bytes so the buffer's CRC-16 equals `target`
    /// (CRC-16 is a linear code, so two free bytes always reach any value;
    /// found by scanning the 65536 candidates against the prefix CRC).
    fn force_crc16(data: &mut [u8], target: u16) {
        let split = data.len() - 2;
        let prefix = lha::crc16(&data[..split]);
        for candidate in 0..=u16::MAX {
            let bytes = candidate.to_le_bytes();
            let mut crc = prefix;
            for &b in &bytes {
                crc ^= b as u16;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xA001
                    } else {
                        crc >> 1
                    };
                }
            }
            if crc == target {
                data[split..].copy_from_slice(&bytes);
                return;
            }
        }
        unreachable!("two free bytes always reach any CRC-16");
    }

    #[cfg(unix)]
    #[test]
    fn slave_search_skips_symlink_loops() {
        let dir = temp_dir("symlink-loop");
        std::fs::create_dir_all(dir.join("Game")).unwrap();
        std::fs::write(dir.join("Game").join("Test.Slave"), b"x").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("Game").join("loop")).unwrap();
        let slaves = find_slaves(&dir).unwrap();
        assert_eq!(slaves, vec![PathBuf::from("Game/Test.Slave")]);
    }

    #[test]
    fn declared_kick_names_with_separators_are_ignored() {
        assert!(safe_kick_name("kick34005.A500"));
        assert!(!safe_kick_name("kick/34005"));
        assert!(!safe_kick_name("..\\evil"));
        assert!(!safe_kick_name("dev:name"));
        assert!(!safe_kick_name(".."));
        assert!(!safe_kick_name(""));
        let bytes = build_slave(17, 0, 0x80000, 0, Some(("../../evil", 0x40000, 0x1234)));
        let slave = parse_slave(&bytes).unwrap();
        assert!(slave.kicks.is_empty());
    }

    /// A canonical image staged under the declared name must not satisfy a
    /// requirement declaring different content: the failure belongs here,
    /// naming the exact ask, not later inside the guest.
    #[test]
    fn canonical_name_with_wrong_content_does_not_satisfy_a_declared_kick() {
        let dir = temp_dir("wrong-content");
        let mut image = fake_kick(0x40000, 11);
        force_crc16(&mut image, 0xE9C6); // identifies as kick33180.A500
        let kick_dir = dir.join("roms");
        std::fs::create_dir_all(&kick_dir).unwrap();
        std::fs::write(kick_dir.join("some.rom"), &image).unwrap();

        let slave = build_slave(17, 0, 0x80000, 0, Some(("kick33180.A500", 0x40000, 0x0102)));
        let game_lha = dir.join("game.lha");
        std::fs::write(&game_lha, build_lha(&[("Game/Game.Slave", &slave)])).unwrap();

        let opts = Options {
            library: Some(dir.join("library")),
            kickstart_dirs: vec![kick_dir],
            extra_args: None,
            assets: Some(fixture_assets(&dir)),
        };
        let err = format!("{:#}", prepare(&game_lha, &opts).unwrap_err());
        assert!(err.contains("kick33180.A500"), "{err}");
        assert!(err.contains("0102"), "{err}");
    }

    /// The extraction cache is only trusted with its completion marker and
    /// a matching source archive: a markerless directory (an interrupted
    /// extraction) is wiped and redone, and a different archive whose stem
    /// sanitizes to the same library entry is refused.
    #[test]
    fn extraction_reuse_needs_marker_and_matching_source() {
        let dir = temp_dir("extraction-cache");
        let slave = build_slave(10, 0, 0x80000, 0, None);
        let game_lha = dir.join("Test Game.lha");
        std::fs::write(&game_lha, build_lha(&[("Game/Game.Slave", &slave)])).unwrap();
        let opts = Options {
            library: Some(dir.join("library")),
            kickstart_dirs: vec![],
            extra_args: None,
            assets: Some(fixture_assets(&dir)),
        };

        // A leftover game/ without the marker is not trusted.
        let partial = dir.join("library").join("Test_Game").join("game");
        std::fs::create_dir_all(&partial).unwrap();
        std::fs::write(partial.join("junk"), b"stale").unwrap();
        let prepared = prepare(&game_lha, &opts).unwrap();
        assert!(!prepared.game_dir.join("junk").exists());
        assert!(prepared.game_dir.join("Game").join("Game.Slave").exists());

        // The same archive reuses the extraction...
        std::fs::write(prepared.game_dir.join("savegame"), b"save").unwrap();
        let again = prepare(&game_lha, &opts).unwrap();
        assert!(again.game_dir.join("savegame").exists());

        // ...but a different archive colliding on the sanitized stem fails.
        let collider = dir.join("Test_Game.lha");
        std::fs::write(&collider, build_lha(&[("Game/Game.Slave", &slave)])).unwrap();
        let err = format!("{:#}", prepare(&collider, &opts).unwrap_err());
        assert!(err.contains("Test Game.lha"), "{err}");
        assert!(err.contains("rename"), "{err}");
    }

    #[test]
    fn remember_game_records_only_the_game() {
        let mut raw = RawConfig::default();
        remember_game(&mut raw, Path::new("/games/Turrican.lha"));
        assert_eq!(raw.whdload.game.as_deref(), Some("/games/Turrican.lha"));
        assert!(raw.filesys.is_empty());
        assert!(raw.machine.profile.is_none());
    }

    #[test]
    fn apply_to_raw_derives_machine_and_mounts_volumes() {
        let prepared = PreparedGame {
            boot_dir: PathBuf::from("/lib/game/boot"),
            game_dir: PathBuf::from("/lib/game/game"),
            slave_rel: PathBuf::from("Game.Slave"),
            slave: SlaveInfo::default(),
            machine_rom: Some(PathBuf::from(
                "/lib/game/boot/Devs/Kickstarts/kick40068.A1200",
            )),
            staged_kicks: vec![],
        };
        let mut raw = RawConfig::default();
        apply_to_raw(&mut raw, &prepared);
        assert_eq!(raw.machine.profile.as_deref(), Some("A1200"));
        assert_eq!(raw.memory.fast.as_deref(), Some("8M"));
        assert!(raw.rom.as_deref().unwrap().ends_with("kick40068.A1200"));
        assert_eq!(raw.filesys.len(), 2);
        assert_eq!(raw.filesys[0].volume.as_deref(), Some(BOOT_VOLUME));
        assert_eq!(raw.filesys[0].bootpri, Some(BOOT_PRIORITY));
        assert_eq!(raw.filesys[1].volume.as_deref(), Some(GAME_VOLUME));

        // Explicit user machine/rom/memory choices win.
        let mut raw = RawConfig::default();
        raw.machine.profile = Some("A4000".to_string());
        raw.memory.fast = Some("2M".to_string());
        raw.rom = Some("my.rom".to_string());
        apply_to_raw(&mut raw, &prepared);
        assert_eq!(raw.machine.profile.as_deref(), Some("A4000"));
        assert_eq!(raw.memory.fast.as_deref(), Some("2M"));
        assert_eq!(raw.rom.as_deref(), Some("my.rom"));
    }
}
