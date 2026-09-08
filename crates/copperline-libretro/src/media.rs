// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_DISKS: usize = 16;
pub const MAX_ADF: usize = 1_802_240;

pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("opening {}", path.display()))?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "{} exceeds {limit} bytes",
        path.display()
    );
    Ok(bytes)
}

pub fn validate_adf(data: &[u8]) -> Result<()> {
    ensure!(
        matches!(data.len(), 901_120 | 1_802_240),
        "this core supports standard 880 KiB and 1760 KiB ADF images"
    );
    Ok(())
}

pub fn is_whdload(path: &Path) -> bool {
    path.is_dir()
        || path
            .extension()
            .and_then(|s| s.to_str())
            .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "lha" | "lzh" | "zip"))
}

pub fn playlist(path: &Path) -> Result<Vec<PathBuf>> {
    if copperline::config::is_cd_image_path(path)
        || path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("adf"))
    {
        return Ok(vec![path.to_path_buf()]);
    }
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u")),
        "expected an ADF, CD image or M3U file"
    );
    let bytes = read_bounded(path, 64 * 1024)?;
    let text = std::str::from_utf8(&bytes)?.trim_start_matches('\u{feff}');
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut disks = Vec::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        ensure!(
            disks.len() < MAX_DISKS,
            "a playlist can contain at most {MAX_DISKS} disks"
        );
        let disk = parent.join(line);
        ensure!(
            copperline::config::is_cd_image_path(&disk)
                || disk
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("adf")),
            "playlist entries must be ADF or CD images"
        );
        disks.push(disk);
    }
    ensure!(!disks.is_empty(), "the playlist is empty");
    ensure!(
        disks.iter().all(|p| copperline::config::is_cd_image_path(p)
            == copperline::config::is_cd_image_path(&disks[0])),
        "a playlist cannot mix floppies and CDs"
    );
    Ok(disks)
}

pub struct Cd {
    pub path: PathBuf,
    pub sources: Vec<(PathBuf, PathBuf)>,
}

pub struct Disk {
    pub cd: Option<Cd>,
    pub label: PathBuf,
    pub bytes: Vec<u8>,
    pub source_hash: [u8; 32],
    save_path: PathBuf,
    saved_hash: [u8; 32],
}

impl Disk {
    pub fn open(path: &Path, save_dir: &Path) -> Result<Self> {
        if copperline::config::is_cd_image_path(path) {
            let path = path.canonicalize()?;
            let image = copperline::cdrom::CdImage::load(&path)?;
            let mut paths = copperline::cdrom::StatePaths::default();
            let mut sources = Vec::new();
            for source in image.source_paths() {
                let mut file = std::fs::File::open(&source)?;
                let mut hash = Sha256::new();
                let mut buffer = [0; 64 * 1024];
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buffer[..n]);
                }
                let alias = PathBuf::from(format!(
                    "cd-{}",
                    hash.finalize()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                ));
                paths.insert(source.clone(), alias.clone())?;
                sources.push((source, alias));
            }
            let source_hash = Sha256::digest(paths.scope(|| bincode::serialize(&image))?).into();
            return Ok(Self {
                label: path.file_name().unwrap_or_default().into(),
                cd: Some(Cd { path, sources }),
                bytes: Vec::new(),
                source_hash,
                save_path: PathBuf::new(),
                saved_hash: source_hash,
            });
        }
        let original = read_bounded(path, MAX_ADF)?;
        validate_adf(&original)?;
        let source_hash: [u8; 32] = Sha256::digest(&original).into();
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let name: String = name
            .chars()
            .take(60)
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let suffix: String = source_hash[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let save_path = save_dir
            .join("copperline")
            .join(format!("{name}-{suffix}.adf"));
        let bytes = if save_path.exists() {
            read_bounded(&save_path, MAX_ADF)?
        } else {
            original
        };
        validate_adf(&bytes)?;
        let saved_hash = Sha256::digest(&bytes).into();
        Ok(Self {
            cd: None,
            label: path.file_name().unwrap_or_default().into(),
            bytes,
            source_hash,
            save_path,
            saved_hash,
        })
    }

    pub fn persist(&mut self) -> Result<()> {
        if self.cd.is_some() {
            return Ok(());
        }
        let hash: [u8; 32] = Sha256::digest(&self.bytes).into();
        if hash == self.saved_hash {
            return Ok(());
        }
        std::fs::create_dir_all(self.save_path.parent().context("missing save directory")?)?;
        // Write alongside the destination so rename stays on the same volume.
        let temporary = self.save_path.with_extension("adf.tmp");
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&self.bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &self.save_path)
            .with_context(|| format!("saving {}", self.save_path.display()))?;
        self.saved_hash = hash;
        Ok(())
    }
}

pub fn write_save(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("missing save directory")?)?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
