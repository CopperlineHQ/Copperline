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

use crate::archive::{Budget, Limits};

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

/// Visit headers in archive order. The visitor decides whether to decode
/// a file, so listing and single-member lookup can skip unrelated payloads.
fn for_each_entry<R, F>(
    mut reader: delharc::LhaDecodeReader<R>,
    limits: Limits,
    mut visit: F,
) -> Result<()>
where
    R: Read + Send + Sync + 'static,
    F: FnMut(PathBuf, bool, &mut delharc::LhaDecodeReader<R>, &mut Budget) -> Result<()>,
{
    let mut budget = Budget::new(limits);
    loop {
        budget.entry()?;
        let header = reader.header();
        let is_dir = header.is_directory();
        let path = sanitize_member_path(&header.parse_pathname())?;
        visit(path, is_dir, &mut reader, &mut budget)?;
        if !reader.next_file()? {
            return Ok(());
        }
    }
}

fn check_decoder<R: Read>(reader: &delharc::LhaDecodeReader<R>, path: &Path) -> Result<()> {
    if !reader.is_decoder_supported() {
        bail!(
            "archive member {} uses an unsupported compression method",
            path.display()
        );
    }
    Ok(())
}

fn check_crc<R: Read>(reader: &delharc::LhaDecodeReader<R>, path: &Path) -> Result<()> {
    if !reader.crc_is_ok() {
        bail!("archive member {} fails its CRC check", path.display());
    }
    Ok(())
}

fn read_entry<R: Read>(
    reader: &mut delharc::LhaDecodeReader<R>,
    path: &Path,
    budget: &mut Budget,
) -> Result<Vec<u8>> {
    check_decoder(reader, path)?;
    let size = reader.header().original_size;
    let data = budget
        .read(reader, size)
        .with_context(|| format!("decompressing archive member {}", path.display()))?;
    check_crc(reader, path)?;
    Ok(data)
}

/// Read every file member of an `.lha` archive into memory, bounded by
/// [`Limits::default`].
pub fn read_archive(archive: &Path) -> Result<Vec<ArchiveEntry>> {
    read_archive_with_limits(archive, Limits::default())
}

/// [`read_archive`] with an explicit output and entry budget.
pub fn read_archive_with_limits(archive: &Path, limits: Limits) -> Result<Vec<ArchiveEntry>> {
    let reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut entries = Vec::new();
    for_each_entry(reader, limits, |path, is_dir, reader, budget| {
        if !is_dir {
            let data = read_entry(reader, &path, budget)?;
            entries.push(ArchiveEntry { path, data });
        }
        Ok(())
    })
    .with_context(|| format!("reading LHA archive {}", archive.display()))?;
    Ok(entries)
}

/// Read the first matching file, skipping unrelated compressed payloads.
pub fn read_first(archive: &Path, want: impl Fn(&Path) -> bool) -> Result<Option<ArchiveEntry>> {
    read_first_with_limits(archive, want, Limits::default())
}

/// [`read_first`] with an explicit budget. Only the selected member counts
/// towards the byte limits; all visited headers count towards the entry cap.
pub fn read_first_with_limits(
    archive: &Path,
    want: impl Fn(&Path) -> bool,
    limits: Limits,
) -> Result<Option<ArchiveEntry>> {
    let mut reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut budget = Budget::new(limits);
    loop {
        budget.entry()?;
        let header = reader.header();
        let is_dir = header.is_directory();
        let path = sanitize_member_path(&header.parse_pathname())?;
        if !is_dir && want(&path) {
            let data = read_entry(&mut reader, &path, &mut budget)?;
            return Ok(Some(ArchiveEntry { path, data }));
        }
        if !reader.next_file()? {
            return Ok(None);
        }
    }
}

/// List normalized file paths without decompressing their contents.
/// Payload CRCs are checked when a member is read or extracted.
pub fn list_files(archive: &Path) -> Result<Vec<PathBuf>> {
    list_files_with_limits(archive, Limits::default())
}

/// [`list_files`] with an explicit entry budget; no output is decoded.
pub fn list_files_with_limits(archive: &Path, limits: Limits) -> Result<Vec<PathBuf>> {
    let reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut paths = Vec::new();
    for_each_entry(reader, limits, |path, is_dir, _, _| {
        if !is_dir {
            paths.push(path);
        }
        Ok(())
    })?;
    Ok(paths)
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
    read_member_with_limits(archive, member, Limits::default())
}

/// [`read_member`] with an explicit budget, decoding only the requested file.
pub fn read_member_with_limits(archive: &Path, member: &Path, limits: Limits) -> Result<Vec<u8>> {
    let want = fold_components(member);
    if let Some(entry) = read_first_with_limits(archive, |p| fold_components(p) == want, limits)? {
        return Ok(entry.data);
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
    extract_to_dir_with_limits(archive, dest, Limits::default())
}

/// [`extract_to_dir`] with an explicit budget. Files stream to disk; an
/// incomplete member is removed if decompression, its limit, or CRC fails.
pub fn extract_to_dir_with_limits(archive: &Path, dest: &Path, limits: Limits) -> Result<usize> {
    let reader = delharc::parse_file(archive)
        .with_context(|| format!("opening LHA archive {}", archive.display()))?;
    let mut written = 0usize;
    for_each_entry(reader, limits, |path, is_dir, reader, budget| {
        let target = dest.join(&path);
        if is_dir {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating directory {}", target.display()))?;
        } else {
            check_decoder(reader, &path)?;
            let size = reader.header().original_size;
            budget.allowance(size)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directory {}", parent.display()))?;
            }
            let mut out = std::fs::File::create(&target)
                .with_context(|| format!("writing {}", target.display()))?;
            let result = budget
                .copy(reader, &mut out, size)
                .and_then(|_| check_crc(reader, &path));
            drop(out);
            if let Err(error) = result {
                let _ = std::fs::remove_file(&target);
                return Err(error).with_context(|| format!("extracting {}", path.display()));
            }
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

    const SMALL: Limits = Limits {
        member_bytes: 4,
        total_bytes: 6,
        entries: 2,
    };

    #[test]
    fn decompression_limits_cover_reads_and_streamed_extraction() {
        let (dir, archive) = write_temp_lha(&[("one", b"1234"), ("two", b"56")]);
        assert_eq!(read_archive_with_limits(&archive, SMALL).unwrap().len(), 2);
        let out = dir.join("bounded");
        assert_eq!(
            extract_to_dir_with_limits(&archive, &out, SMALL).unwrap(),
            2
        );
        assert_eq!(std::fs::read(out.join("two")).unwrap(), b"56");

        for limits in [
            Limits {
                member_bytes: 3,
                ..SMALL
            },
            Limits {
                total_bytes: 5,
                ..SMALL
            },
            Limits {
                entries: 1,
                ..SMALL
            },
        ] {
            assert!(read_archive_with_limits(&archive, limits).is_err());
            assert!(extract_to_dir_with_limits(&archive, &dir.join("limited"), limits).is_err());
        }
        assert!(read_first_with_limits(
            &archive,
            |_| true,
            Limits {
                member_bytes: 3,
                ..SMALL
            }
        )
        .is_err());
    }

    #[test]
    fn listing_and_lookup_skip_unrelated_payloads() {
        let (_dir, archive) = write_temp_lha(&[("data", b"oversize"), ("loader", b"ok")]);
        // Break the unrelated member's CRC. Header-only listing and a
        // selective lookup must not decode it, even under a tiny budget.
        let mut bytes = std::fs::read(&archive).unwrap();
        let data_start = usize::from(bytes[0]) + 2;
        bytes[data_start] ^= 0xff;
        std::fs::write(&archive, bytes).unwrap();
        assert_eq!(list_files_with_limits(&archive, SMALL).unwrap().len(), 2);
        assert_eq!(
            read_member_with_limits(&archive, Path::new("LOADER"), SMALL).unwrap(),
            b"ok"
        );
        assert!(read_archive_with_limits(&archive, SMALL).is_err());
        assert!(list_files_with_limits(
            &archive,
            Limits {
                entries: 1,
                ..SMALL
            }
        )
        .is_err());
    }

    #[test]
    fn directory_headers_count_towards_the_entry_limit() {
        let (dir, archive) = write_temp_lha(&[("one", b""), ("two", b""), ("three", b"")]);
        let mut bytes = std::fs::read(&archive).unwrap();
        let mut at = 0;
        while bytes[at] != 0 {
            let end = at + usize::from(bytes[at]) + 2;
            bytes[at + 2..at + 7].copy_from_slice(b"-lhd-");
            bytes[at + 1] = bytes[at + 2..end]
                .iter()
                .fold(0u8, |sum, &b| sum.wrapping_add(b));
            at = end;
        }
        std::fs::write(&archive, bytes).unwrap();
        assert!(list_files_with_limits(&archive, SMALL).is_err());
        assert!(read_first_with_limits(&archive, |_| false, SMALL).is_err());
        assert!(extract_to_dir_with_limits(&archive, &dir.join("dirs"), SMALL).is_err());
    }

    #[test]
    fn a_failed_crc_removes_the_incomplete_extracted_member() {
        let (dir, archive) = write_temp_lha(&[("file", b"data")]);
        let mut bytes = std::fs::read(&archive).unwrap();
        let at = bytes.len() - 2;
        bytes[at] ^= 0xff;
        std::fs::write(&archive, bytes).unwrap();
        let out = dir.join("bad-crc");
        assert!(extract_to_dir_with_limits(&archive, &out, SMALL).is_err());
        assert!(!out.join("file").exists());
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
