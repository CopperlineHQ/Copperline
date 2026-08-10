// SPDX-License-Identifier: GPL-3.0-or-later

//! What a WHDLoad game arrives as, and how to get at it.
//!
//! Three shapes, and a person should not have to care which they have:
//!
//! - **`.lha`**, which is what the packagers publish and what an Amiga
//!   would have read natively.
//! - **`.zip`**, which is what a browser hands you when you download a
//!   folder, and what somebody who has unpacked and repacked one ends up
//!   with.
//! - **a plain folder**, which is what you get when you unpack either.
//!
//! The `.slave` is what makes any of them a game. Where it sits varies:
//! straight in the archive root for a published `.lha`, a folder or two
//! down for a zip made from an unpacked one. Nothing here cares -- the
//! slave is searched for, not assumed.
//!
//! Two kinds of rubbish are ignored throughout. macOS writes `__MACOSX/`
//! and `._name` AppleDouble stubs into any zip it makes, and one of those
//! stubs is called `._Something.Slave`: left in, it would be found by the
//! slave search, sort before the real one, and be booted instead. `.DS_Store`
//! is harmless but there is no reason to stage it onto a mounted volume.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::lha;

/// Which of the three a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Lha,
    Zip,
    Folder,
}

/// The extensions a package file may carry, lower case.
///
/// A closed list, checked case-insensitively, rather than "whatever
/// follows the last dot". Amiga titles are full of dots -- `S.W.I.V`,
/// `R.B.I. 2 Baseball`, `Dr.Plummet` -- so treating the tail as an
/// extension mangles the name of anything that has one and no extension,
/// which is every game that arrives as a plain folder. Every launcher that
/// scans a library does it this way for the same reason.
pub const EXTENSIONS: [&str; 3] = ["lha", "lzh", "zip"];

/// What a slave file is called. `.slav` as well as `.slave`, because a
/// package that has been through a filesystem with a short name limit
/// comes out that way and is still a game.
pub const SLAVE_EXTENSIONS: [&str; 2] = ["slave", "slav"];

/// Directory names never worth walking into: version control, and the two
/// Copperline itself writes inside a library.
const SKIP_DIRS: [&str; 5] = [".git", "__MACOSX", "Cache", "Save States", "Saves"];

impl Kind {
    /// What `path` looks like, by extension for a file and by being one
    /// for a directory. `None` for anything else, which is how a folder of
    /// games tells a game from a text file.
    pub fn of(path: &Path) -> Option<Kind> {
        if path.is_dir() {
            return Some(Kind::Folder);
        }
        Kind::of_name(path.file_name()?.to_str()?)
    }

    /// The same from a name alone, for deciding what to list without
    /// asking the filesystem twice. Never answers `Folder`: a name does
    /// not say whether it is one.
    pub fn of_name(name: &str) -> Option<Kind> {
        match extension_of(name)? {
            "lha" | "lzh" => Some(Kind::Lha),
            "zip" => Some(Kind::Zip),
            _ => None,
        }
    }
}

/// The package extension a name ends in, lower case, or `None`.
///
/// Only the ones in [`EXTENSIONS`]: `Aladdin_v1` has no extension, and
/// `S.W.I.V` has a dot but no extension either.
fn extension_of(name: &str) -> Option<&'static str> {
    let (_, tail) = name.rsplit_once('.')?;
    EXTENSIONS
        .iter()
        .find(|ext| tail.eq_ignore_ascii_case(ext))
        .copied()
}

/// A package's name without its extension, if it has one this recognises.
///
/// `GoldenAxe_v1.5_0017.lha` loses the `.lha`; `S.W.I.V` and `Aladdin_v1`
/// keep every character, because neither ends in an extension.
pub fn stem_of(name: &str) -> &str {
    match extension_of(name) {
        Some(ext) => &name[..name.len() - ext.len() - 1],
        None => name,
    }
}

/// A stored relative path resolved under `folder`.
///
/// The store writes `/` whatever the host uses, so a library scanned on
/// one machine still matches on another. Windows takes `/` in a path
/// happily enough, but joining a component at a time says so outright
/// rather than relying on it, and what comes out carries the host's own
/// separator for everything downstream.
pub fn under(folder: &Path, relative: &str) -> PathBuf {
    let mut at = folder.to_path_buf();
    at.extend(relative.split('/').filter(|part| !part.is_empty()));
    at
}

/// Whether a name is a WHDLoad slave.
pub fn is_slave_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, tail)| {
        SLAVE_EXTENSIONS
            .iter()
            .any(|e| tail.eq_ignore_ascii_case(e))
    })
}

/// Whether a directory is worth walking into while looking for games.
pub fn worth_walking(name: &str) -> bool {
    !SKIP_DIRS.contains(&name) && !name.starts_with('.')
}

/// Whether a member path is rubbish an archiver added rather than part of
/// the game.
///
/// `__MACOSX/._Foo.Slave` is the one that matters: it ends in `.Slave`, so
/// a slave search finds it, and it sorts before the real one often enough
/// to be booted instead. It is not a slave -- it is a few hundred bytes of
/// resource fork -- so the game would fail to start with nothing obvious
/// to blame.
pub fn is_rubbish(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        name == "__MACOSX" || name == ".DS_Store" || name.starts_with("._")
    })
}

/// Unpack an archive into `dest`, answering how many files it wrote.
///
/// Only for the two archive kinds; a folder is already unpacked and the
/// caller uses it where it is.
pub fn extract_to_dir(archive: &Path, dest: &Path) -> Result<usize> {
    match Kind::of(archive) {
        Some(Kind::Lha) => lha::extract_to_dir(archive, dest),
        Some(Kind::Zip) => extract_zip(archive, dest),
        Some(Kind::Folder) => bail!("{} is a directory, not an archive", archive.display()),
        None => bail!(
            "{} is not a WHDLoad package (.lha, .zip or a folder)",
            archive.display()
        ),
    }
}

/// The relative paths of the members of an archive, or of the files in a
/// folder, with the rubbish left out.
pub fn list(path: &Path) -> Result<Vec<PathBuf>> {
    let all = match Kind::of(path) {
        Some(Kind::Lha) => lha::list_files(path)?,
        Some(Kind::Zip) => zip_names(path)?,
        Some(Kind::Folder) => walk(path)?,
        None => bail!("{} is not a WHDLoad package", path.display()),
    };
    Ok(all.into_iter().filter(|p| !is_rubbish(p)).collect())
}

/// Read one member out of an archive or folder, by a path `list` returned.
pub fn read(path: &Path, member: &Path) -> Result<Vec<u8>> {
    match Kind::of(path) {
        Some(Kind::Lha) => lha::read_member(path, member),
        Some(Kind::Zip) => read_zip_member(path, member),
        Some(Kind::Folder) => std::fs::read(path.join(member))
            .with_context(|| format!("reading {}", path.join(member).display())),
        None => bail!("{} is not a WHDLoad package", path.display()),
    }
}

/// The first member whose path satisfies `want`, reading only that one.
///
/// The point of it is the scan, which wants one small `.slave` out of each
/// of a few thousand packages and has no use for the rest.
pub fn read_first(path: &Path, want: impl Fn(&Path) -> bool) -> Result<Option<Vec<u8>>> {
    let wanted = |p: &Path| !is_rubbish(p) && want(p);
    match Kind::of(path) {
        Some(Kind::Lha) => Ok(lha::read_first(path, wanted)?.map(|entry| entry.data)),
        Some(Kind::Zip) => {
            let mut zip = open_zip(path)?;
            let found = (0..zip.len()).find_map(|i| {
                let file = zip.by_index(i).ok()?;
                let member = member_path(file.name())?;
                (!file.is_dir() && wanted(&member)).then_some(i)
            });
            match found {
                Some(i) => {
                    let mut data = Vec::new();
                    zip.by_index(i)
                        .with_context(|| format!("reading a member of {}", path.display()))?
                        .read_to_end(&mut data)
                        .with_context(|| format!("decompressing a member of {}", path.display()))?;
                    Ok(Some(data))
                }
                None => Ok(None),
            }
        }
        Some(Kind::Folder) => {
            for member in walk(path)? {
                if wanted(&member) {
                    return Ok(Some(std::fs::read(path.join(&member))?));
                }
            }
            Ok(None)
        }
        None => Ok(None),
    }
}

/// Whether a `.slave` sits anywhere within `dir`, within a few levels.
///
/// This is the test a scan applies to the *children* of a game folder, and
/// never to the folder itself -- a library holds games, so a slave is
/// somewhere under it too, and asking of the library would answer yes and
/// mean nothing. Walking down and stopping at the first child that says
/// yes is what picks out `Aladdin_v1/` from a folder of a hundred like it.
///
/// The depth limit is what stops the question from reading somebody's
/// whole home directory when the answer is no.
pub fn holds_a_slave(dir: &Path) -> bool {
    fn look(dir: &Path, depth: usize) -> bool {
        if depth > FOLDER_SLAVE_DEPTH {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        let mut subdirs = Vec::new();
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("._") {
                continue;
            }
            if kind.is_dir() {
                if !worth_walking(&name) {
                    continue;
                }
                subdirs.push(entry.path());
            } else if is_slave_name(&name) {
                return true;
            }
        }
        subdirs.iter().any(|sub| look(sub, depth + 1))
    }
    look(dir, 0)
}

/// How far under a directory a slave may be for the directory to count as
/// the game. Two is what the real layouts need -- `Game_v1/Game/Game.slave`
/// -- and a third is slack for anyone who nested one deeper.
const FOLDER_SLAVE_DEPTH: usize = 3;

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::io::BufReader<std::fs::File>>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening ZIP archive {}", path.display()))?;
    zip::ZipArchive::new(std::io::BufReader::new(file))
        .with_context(|| format!("reading ZIP archive {}", path.display()))
}

/// A member name as a relative path, or `None` if it tries to leave the
/// archive.
///
/// A ZIP stores whatever string the writer chose, which may be absolute or
/// full of `..`. Unpacking one of those writes outside the destination,
/// which is the oldest bug in archive handling and still worth not having.
fn member_path(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            part if part.contains(':') => return None,
            part => out.push(part),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn zip_names(path: &Path) -> Result<Vec<PathBuf>> {
    let mut zip = open_zip(path)?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let file = zip
            .by_index(i)
            .with_context(|| format!("reading a member of {}", path.display()))?;
        if file.is_dir() {
            continue;
        }
        match member_path(file.name()) {
            Some(member) => out.push(member),
            None => log::warn!(
                "whdload: {} has a member named {:?}, which is not a relative path; skipped",
                path.display(),
                file.name()
            ),
        }
    }
    Ok(out)
}

fn read_zip_member(path: &Path, member: &Path) -> Result<Vec<u8>> {
    let mut zip = open_zip(path)?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if member_path(file.name()).as_deref() == Some(member) {
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    bail!("{} has no member {}", path.display(), member.display())
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<usize> {
    let mut zip = open_zip(archive)?;
    let mut written = 0usize;
    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .with_context(|| format!("reading a member of {}", archive.display()))?;
        if file.is_dir() {
            continue;
        }
        let Some(member) = member_path(file.name()) else {
            log::warn!(
                "whdload: {} has a member named {:?}, which is not a relative path; skipped",
                archive.display(),
                file.name()
            );
            continue;
        };
        if is_rubbish(&member) {
            continue;
        }
        let at = dest.join(&member);
        if let Some(parent) = at.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out =
            std::fs::File::create(&at).with_context(|| format!("writing {}", at.display()))?;
        std::io::copy(&mut file, &mut out)
            .with_context(|| format!("extracting {}", member.display()))?;
        written += 1;
    }
    Ok(written)
}

/// Every file under a directory, relative to it, links not followed.
///
/// Ordered shallowest first and then by name, which is the order
/// `whdload::find_slaves` prefers a slave in -- and, more to the point,
/// an order at all: a directory is read in whatever order the filesystem
/// feels like, so an unsorted walk would hash a different slave on
/// different runs and the digest that identifies a package would move
/// under it.
fn walk(root: &Path) -> Result<Vec<PathBuf>> {
    fn step(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        let entries =
            std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
        for entry in entries.flatten() {
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                step(root, &path, out)?;
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    step(root, root, &mut out)?;
    out.sort_by_key(|p| (p.components().count(), p.to_string_lossy().to_lowercase()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_package_looks_like() {
        assert_eq!(Kind::of_name("Zool_v1.0.lha"), Some(Kind::Lha));
        assert_eq!(Kind::of_name("Zool_v1.0.LHA"), Some(Kind::Lha));
        assert_eq!(Kind::of_name("Zool_v1.zip"), Some(Kind::Zip));
        assert_eq!(Kind::of_name("Zool_v1.ZIP"), Some(Kind::Zip));
        assert_eq!(Kind::of_name("notes.txt"), None);
        assert_eq!(Kind::of_name("noextension"), None);
    }

    #[test]
    fn a_resource_fork_is_not_a_slave() {
        // The one that matters: macOS puts `._Foo.Slave` in every zip it
        // makes, it ends in `.Slave`, and it sorts before the real one. A
        // game booted from one starts and immediately fails.
        assert!(is_rubbish(Path::new("__MACOSX/Aladdin/._AladdinAGA.Slave")));
        assert!(is_rubbish(Path::new("Aladdin/._AladdinAGA.Slave")));
        assert!(is_rubbish(Path::new("Aladdin/.DS_Store")));
        assert!(is_rubbish(Path::new("__MACOSX/anything")));
        assert!(!is_rubbish(Path::new("Aladdin/AladdinAGA.Slave")));
        assert!(!is_rubbish(Path::new("Aladdin/data/_notes")));
    }

    #[test]
    fn a_stored_path_resolves_under_the_folder() {
        // The store keeps `/` whatever the host wrote it on.
        let at = under(Path::new("/games"), "A/Zool_v1.0.lha");
        assert_eq!(at, Path::new("/games").join("A").join("Zool_v1.0.lha"));
        assert_eq!(
            under(Path::new("/games"), "Zool.lha"),
            Path::new("/games").join("Zool.lha")
        );
        // Nothing odd from an empty or slash-heavy relative.
        assert_eq!(under(Path::new("/games"), ""), Path::new("/games"));
        assert_eq!(
            under(Path::new("/games"), "A//B.lha"),
            Path::new("/games").join("A").join("B.lha")
        );
    }

    #[test]
    fn a_member_never_leaves_the_archive() {
        // A ZIP stores whatever string the writer chose.
        assert_eq!(
            member_path("Game/Game.slave").as_deref(),
            Some(Path::new("Game/Game.slave"))
        );
        assert_eq!(
            member_path("Game\\Game.slave").as_deref(),
            Some(Path::new("Game/Game.slave"))
        );
        assert_eq!(
            member_path("./Game/./Game.slave").as_deref(),
            Some(Path::new("Game/Game.slave"))
        );
        // Absolute, traversing, or an AmigaDOS device name.
        assert_eq!(member_path("../../etc/passwd"), None);
        assert_eq!(member_path("Game/../../etc/passwd"), None);
        assert_eq!(member_path("DH0:Game/Game.slave"), None);
        assert_eq!(member_path(""), None);
        assert_eq!(member_path("/"), None);
        // A leading slash is just an empty first part, which is skipped --
        // the result is still relative.
        assert_eq!(
            member_path("/Game/Game.slave").as_deref(),
            Some(Path::new("Game/Game.slave"))
        );
    }

    #[test]
    fn a_folder_is_read_in_a_settled_order() {
        // A directory is read in whatever order the filesystem feels
        // like. The digest that identifies a package is taken from the
        // first slave found, so an unsettled order would move it between
        // runs and a rename would stop being recognisable.
        let dir = std::env::temp_dir().join(format!("copperline-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data/deep")).unwrap();
        for at in ["data/deep/Zzz.slave", "data/Bbb.slave", "Aaa.slave"] {
            std::fs::write(dir.join(at), b"x").unwrap();
        }
        // Compared as paths, not as text: `display()` uses the host's
        // separator, so a string comparison here would pass on Unix and
        // fail on Windows for no reason anybody cares about.
        let listed = walk(&dir).unwrap();
        assert_eq!(
            listed,
            ["Aaa.slave", "data/Bbb.slave", "data/deep/Zzz.slave"].map(PathBuf::from)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_game_folder_is_picked_out_of_a_library() {
        let dir = std::env::temp_dir().join(format!("copperline-package-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // The real layout: the library holds game folders, and the slave is
        // two levels below the game folder.
        std::fs::create_dir_all(dir.join("Aladdin_v1/AladdinAGA")).unwrap();
        std::fs::write(dir.join("Aladdin_v1/AladdinAGA/AladdinAGA.Slave"), b"x").unwrap();
        // The library says yes as well -- which is why it is asked of the
        // children and the walk stops at the first that agrees.
        assert!(holds_a_slave(&dir.join("Aladdin_v1")));
        assert!(holds_a_slave(&dir.join("Aladdin_v1/AladdinAGA")));

        // A folder with no slave under it at all is not a game.
        std::fs::create_dir_all(dir.join("Screenshots")).unwrap();
        std::fs::write(dir.join("Screenshots/one.png"), b"x").unwrap();
        assert!(!holds_a_slave(&dir.join("Screenshots")));

        // A resource fork does not make one, either.
        std::fs::create_dir_all(dir.join("Junk/__MACOSX")).unwrap();
        std::fs::write(dir.join("Junk/__MACOSX/._Game.Slave"), b"x").unwrap();
        assert!(!holds_a_slave(&dir.join("Junk")));

        // Nor does one buried far deeper than a package ever puts it.
        std::fs::create_dir_all(dir.join("Deep/a/b/c/d")).unwrap();
        std::fs::write(dir.join("Deep/a/b/c/d/Game.slave"), b"x").unwrap();
        assert!(!holds_a_slave(&dir.join("Deep")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
