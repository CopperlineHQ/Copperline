// SPDX-License-Identifier: GPL-3.0-or-later

//! LHA/LZH archive reading, wrapped around the `delharc` decoder.
//!
//! Amiga software is customarily distributed as `.lha` archives; WHDLoad
//! game packages in particular are LhA trees carrying a `.slave` loader
//! next to the game data (src/whdload.rs). Amiga LhA wrote header levels
//! 0-2 with `-lh0-`/`-lh1-`/`-lh5-` members and `/`-separated subpaths,
//! all of which delharc handles; entry CRCs are verified during
//! extraction.
//!
//! Archive member paths are normalized before use: absolute prefixes and
//! `..` components are rejected so a hostile archive cannot escape its
//! extraction directory.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The ANSI CRC-16 (polynomial $8005, reflected as $A001, init 0) used both
/// by the LHA entry checksums and by WHDLoad to identify Kickstart images
/// (`resload_CRC16` in the WHDLoad autodocs calls it "ANSI conform").
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= byte as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// One extracted archive member: its normalized relative path and contents.
pub struct ArchiveEntry {
    pub path: PathBuf,
    pub data: Vec<u8>,
}

/// Reject absolute or parent-escaping member paths and collapse the rest to
/// a plain relative path.
fn sanitize_member_path(raw: &Path) -> Result<PathBuf> {
    let mut clean = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                bail!(
                    "archive member path {} is not a safe relative path",
                    raw.display()
                )
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("archive member has an empty path");
    }
    Ok(clean)
}

/// Walk every member of an LHA archive, handing decompressed entries to
/// `visit`. Directory placeholders (`-lhd-`) are reported with empty data so
/// callers can create them; unsupported compression methods are an error
/// (Amiga archives use lh0/lh1/lh5, all supported).
fn for_each_entry<R, F>(mut reader: delharc::LhaDecodeReader<R>, mut visit: F) -> Result<()>
where
    R: Read + Send + Sync + 'static,
    F: FnMut(ArchiveEntry, bool) -> Result<()>,
{
    loop {
        let header = reader.header();
        let raw_path = header.parse_pathname();
        let is_dir = header.is_directory();
        let path = sanitize_member_path(&raw_path)?;
        if is_dir {
            visit(
                ArchiveEntry {
                    path,
                    data: Vec::new(),
                },
                true,
            )?;
        } else {
            if !reader.is_decoder_supported() {
                bail!(
                    "archive member {} uses an unsupported compression method",
                    path.display()
                );
            }
            let mut data = Vec::new();
            reader
                .read_to_end(&mut data)
                .with_context(|| format!("decompressing archive member {}", path.display()))?;
            if !reader.crc_is_ok() {
                bail!("archive member {} fails its CRC check", path.display());
            }
            visit(ArchiveEntry { path, data }, false)?;
        }
        if !reader.next_file()? {
            return Ok(());
        }
    }
}

/// Read every file member of an `.lha` archive into memory.
pub fn read_archive(archive: &Path) -> Result<Vec<ArchiveEntry>> {
    let reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut entries = Vec::new();
    for_each_entry(reader, |entry, is_dir| {
        if !is_dir {
            entries.push(entry);
        }
        Ok(())
    })
    .with_context(|| format!("reading LHA archive {}", archive.display()))?;
    Ok(entries)
}

/// List the (normalized, relative) file member paths of an `.lha` archive
/// without keeping their contents.
pub fn list_files(archive: &Path) -> Result<Vec<PathBuf>> {
    Ok(read_archive(archive)?.into_iter().map(|e| e.path).collect())
}

/// A path lowered to its components for host-neutral comparison: rendering
/// a `PathBuf` as a string would join with the HOST separator (`\` on
/// Windows), which can never match a `/`-separated query string.
fn fold_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect()
}

/// Extract a single member, matched against `member` case-insensitively the
/// way AmigaDOS names are matched (archives are inconsistent about case),
/// and component-wise so the host's path separator does not matter.
pub fn read_member(archive: &Path, member: &Path) -> Result<Vec<u8>> {
    let want = fold_components(member);
    let entries = read_archive(archive)?;
    for entry in entries {
        if fold_components(&entry.path) == want {
            return Ok(entry.data);
        }
    }
    bail!(
        "archive {} has no member {}",
        archive.display(),
        member.display()
    )
}

/// Extract a whole archive into `dest`, creating subdirectories as needed.
/// Returns the number of files written. Existing files are overwritten:
/// the caller decides whether a fresh extraction is wanted at all.
pub fn extract_to_dir(archive: &Path, dest: &Path) -> Result<usize> {
    let reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut written = 0usize;
    for_each_entry(reader, |entry, is_dir| {
        let target = dest.join(&entry.path);
        if is_dir {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating directory {}", target.display()))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directory {}", parent.display()))?;
            }
            std::fs::write(&target, &entry.data)
                .with_context(|| format!("writing {}", target.display()))?;
            written += 1;
        }
        Ok(())
    })
    .with_context(|| format!("extracting LHA archive {}", archive.display()))?;
    Ok(written)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encode one level-0 `-lh0-` (stored) member. Amiga LhA wrote level-0
    /// headers with `/`-separated subpaths, which is exactly what this
    /// produces, so the fixtures mirror real WHDLoad packages.
    pub(crate) fn lh0_member(path: &str, data: &[u8]) -> Vec<u8> {
        let name = path.as_bytes();
        let mut body = Vec::new();
        body.extend_from_slice(b"-lh0-");
        body.extend_from_slice(&(data.len() as u32).to_le_bytes()); // packed
        body.extend_from_slice(&(data.len() as u32).to_le_bytes()); // original
        body.extend_from_slice(&[0, 0, 0, 0]); // MS-DOS time
        body.push(0x20); // attribute
        body.push(0); // header level
        body.push(name.len() as u8);
        body.extend_from_slice(name);
        body.extend_from_slice(&crc16(data).to_le_bytes());
        let mut out = Vec::new();
        out.push(body.len() as u8);
        out.push(body.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)));
        out.extend_from_slice(&body);
        out.extend_from_slice(data);
        out
    }

    /// Build a whole in-memory `.lha` from (path, data) pairs.
    pub(crate) fn build_lha(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (path, data) in members {
            out.extend_from_slice(&lh0_member(path, data));
        }
        out.push(0); // archive terminator
        out
    }

    /// Unique scratch directory following the suite's temp-path convention
    /// (process id + nanos, no tempfile dependency). Callers let it leak;
    /// the OS temp dir is periodically cleaned by the host.
    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "copperline-lha-test-{}-{nanos}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_temp_lha(members: &[(&str, &[u8])]) -> (PathBuf, PathBuf) {
        let dir = temp_dir("fixture");
        let path = dir.join("fixture.lha");
        std::fs::write(&path, build_lha(members)).unwrap();
        (dir, path)
    }

    #[test]
    fn crc16_matches_known_ansi_vector() {
        // CRC-16/ARC check value for "123456789".
        assert_eq!(crc16(b"123456789"), 0xBB3D);
    }

    #[test]
    fn stored_members_round_trip_with_subpaths() {
        let (dir, archive): (PathBuf, PathBuf) = write_temp_lha(&[
            ("Game.info", b"icon"),
            ("Game/Game.Slave", b"slave-bytes"),
            ("Game/data/level1", b"payload"),
        ]);
        let entries = read_archive(&archive).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].path, PathBuf::from("Game/Game.Slave"));
        assert_eq!(entries[1].data, b"slave-bytes");

        let out = dir.join("out");
        assert_eq!(extract_to_dir(&archive, &out).unwrap(), 3);
        assert_eq!(
            std::fs::read(out.join("Game/data/level1")).unwrap(),
            b"payload"
        );
    }

    /// The comparison must survive a `PathBuf` assembled with the host's
    /// own separator (backslash on Windows) against a `/`-separated query;
    /// comparing rendered strings did not, which broke every archive-member
    /// lookup on Windows.
    #[test]
    fn member_lookup_is_separator_neutral() {
        let entry_path: PathBuf = ["WHDLoad", "C", "WHDLoad"].iter().collect();
        assert_eq!(
            fold_components(&entry_path),
            fold_components(Path::new("whdload/c/whdload"))
        );
    }

    #[test]
    fn member_lookup_is_case_insensitive() {
        let (_dir, archive) = write_temp_lha(&[("WHDLoad/C/WHDLoad", b"prog")]);
        let data = read_member(&archive, Path::new("whdload/c/whdload")).unwrap();
        assert_eq!(data, b"prog");
        assert!(read_member(&archive, Path::new("missing")).is_err());
    }

    #[test]
    fn corrupt_member_crc_is_rejected() {
        let (_dir, archive) = write_temp_lha(&[("file", b"payload")]);
        let mut bytes = std::fs::read(&archive).unwrap();
        let len = bytes.len();
        bytes[len - 2] ^= 0xFF; // last data byte of the stored member
        std::fs::write(&archive, bytes).unwrap();
        assert!(read_archive(&archive).is_err());
    }

    /// delharc's own `parse_pathname` already strips `..` and root
    /// components (verified: a member named `../evil` parses as `evil`), so
    /// the guard in `sanitize_member_path` is defense in depth exercised
    /// directly here.
    #[test]
    fn escaping_member_paths_are_rejected() {
        assert!(sanitize_member_path(Path::new("../evil")).is_err());
        assert!(sanitize_member_path(Path::new("/abs/evil")).is_err());
        assert!(sanitize_member_path(Path::new("a/../../evil")).is_err());
        assert!(sanitize_member_path(Path::new("")).is_err());
        assert_eq!(
            sanitize_member_path(Path::new("./Game/data")).unwrap(),
            PathBuf::from("Game/data")
        );
    }
}
