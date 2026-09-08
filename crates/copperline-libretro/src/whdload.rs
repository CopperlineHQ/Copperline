// SPDX-License-Identifier: GPL-3.0-or-later

use anyhow::{ensure, Result};
use copperline::config::{Config, DriveImage};
use copperline::diskimage::FileSystem;
use copperline::whdload::{self, Options, WhdbootAssets};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct Prepared {
    // Keep the staged files alive until the session-backed disks have opened.
    pub _temporary: tempfile::TempDir,
    pub identity: [u8; 32],
    pub capacity: usize,
    pub saves: [PathBuf; 2],
}

pub fn prepare(
    config: &mut Config,
    content: &Path,
    system: &Path,
    save: &Path,
) -> Result<Prepared> {
    let temporary = tempfile::tempdir()?;
    let support = system.join("whdboot");
    let game = whdload::prepare(
        content,
        &Options {
            library: Some(temporary.path().join("library")),
            kickstart_dirs: vec![system.join("Kickstarts"), system.to_path_buf()],
            extra_args: None,
            assets: Some(WhdbootAssets {
                whdload_archive: support.join(whdload::WHDLOAD_USR_ARCHIVE),
                skick_archive: Some(support.join(whdload::SKICK_ARCHIVE)),
            }),
        },
    )?;
    config.fast_ram_bytes = 8 * 1024 * 1024;
    config.netplay_storage = true;
    let mut hash = Sha256::new();
    let mut capacity = 0;
    let mut drives = Vec::new();
    let mut image_names = Vec::new();
    for (index, (dir, volume, priority)) in [
        (&game.boot_dir, whdload::BOOT_VOLUME, 6),
        (
            &game.game_dir,
            whdload::GAME_VOLUME,
            copperline::config::BOOT_PRI_NEVER,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bytes = copperline::dirfs::build_image_at(
            dir,
            volume,
            FileSystem::OFS,
            128 * 1024 * 1024,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800),
        )?;
        let digest = Sha256::digest(&bytes);
        hash.update(digest);
        image_names.push(
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        );
        // Sparse sector records cost slightly more than their data. Reserve
        // enough for every sector to change without increasing serialize_size.
        capacity += bytes.len() * 2;
        let path = temporary.path().join(format!("{index}.hdf"));
        std::fs::write(&path, bytes)?;
        drives.push(DriveImage {
            path,
            volume_name: Some(volume.into()),
            boot_pri: priority,
            filesystem: FileSystem::OFS,
        });
    }
    ensure!(
        config.ide.master.is_none() && config.ide.slave.is_none(),
        "WHDLoad needs both IDE slots"
    );
    config.ide.master = Some(drives.remove(0));
    config.ide.slave = Some(drives.remove(0));
    let identity: [u8; 32] = hash.finalize().into();
    // Updating the support archives changes the boot disk but must not hide
    // writes to an unchanged game volume.
    let root = save
        .join("copperline")
        .join("whdload")
        .join(&image_names[1]);
    Ok(Prepared {
        _temporary: temporary,
        identity,
        capacity,
        saves: [
            root.join(format!("boot-{}.overlay", image_names[0])),
            root.join("game.overlay"),
        ],
    })
}
