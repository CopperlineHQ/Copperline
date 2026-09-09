// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated checks of the CHD hard-disk backend against a real chdman
//! image: `AmigaSYS3PlusAGA-rdb.chd`, made from the AmigaSYS RDB hardfile
//! with `chdman createhd -i AmigaSYS3PlusAGA-rdb.hdf -o
//! AmigaSYS3PlusAGA-rdb.chd`. The unit tests in `src/harddrive/chd.rs`
//! cover addressing, the overlay, and header validation with synthesized
//! uncompressed CHDs; these prove the compressed-codec path (LZMA/Deflate
//! hunks, a real 25,000-hunk map) against the hardfile it came from, and
//! boot AmigaOS from it.

use copperline::diskimage::FileSystem;
use copperline::harddrive::{chd::overlay_path, HardDriveImage, SECTOR_SIZE};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The integration-test asset directory (see `tests/README.md`):
/// `COPPERLINE_TEST_ASSETS`, else `test-assets/` under the repo root,
/// else the repo root itself.
fn asset_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match std::env::var_os("COPPERLINE_TEST_ASSETS") {
        Some(d) => PathBuf::from(d),
        None => {
            let d = root.join("test-assets");
            if d.is_dir() {
                d
            } else {
                root
            }
        }
    }
}

fn asset(name: &str) -> Option<PathBuf> {
    let path = asset_dir().join(name);
    path.is_file().then_some(path)
}

/// A scratch directory holding a private copy of the CHD, so the overlay
/// sidecar the drive creates lands beside the copy and never in the asset
/// directory.
fn scratch_copy(source: &Path) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "copperline-chd-hdd-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("work.chd");
    std::fs::copy(source, &copy).unwrap();
    (dir, copy)
}

fn open(path: &Path) -> HardDriveImage {
    HardDriveImage::open(path, "DH0", "ide", None, 0, FileSystem::FFS).expect("image opens")
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn distinct_colors(path: &Path) -> usize {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut data = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut data).unwrap();
    let stride = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("unexpected screenshot pixel format {other:?}"),
    };
    data[..info.buffer_size()]
        .chunks_exact(stride)
        .map(|pixel| pixel[..3].to_vec())
        .collect::<HashSet<_>>()
        .len()
}

#[test]
#[ignore = "needs the AmigaSYS RDB hardfile and its chdman conversion (see tests/README.md)"]
fn chd_hard_disk_matches_its_source_hdf_and_takes_writes_in_the_overlay() {
    let (Some(chd), Some(hdf)) = (
        asset("AmigaSYS3PlusAGA-rdb.chd"),
        asset("AmigaSYS3PlusAGA-rdb.hdf"),
    ) else {
        eprintln!("skipping: AmigaSYS CHD/HDF pair not in the asset directory");
        return;
    };
    let (dir, copy) = scratch_copy(&chd);
    let pristine = std::fs::read(&copy).unwrap();

    let mut source = open(&hdf);
    let mut disk = open(&copy);
    assert!(disk.has_own_rdb(), "the RDB image is served as-is");
    assert!(!disk.write_protected(), "the overlay opened");
    // chdman pads the last cylinder; the disk is never smaller than its source.
    assert!(disk.total_sectors() >= source.total_sectors());

    // Every sector of the RDB and the start of the first partition, a
    // stride across the whole disk, and the tail, against the hardfile.
    let last = source.total_sectors() - 1;
    let mut lbas: Vec<u64> = (0..8192).collect();
    lbas.extend((0..last).step_by(4099));
    lbas.extend(last.saturating_sub(64)..=last);
    let mut a = vec![0u8; SECTOR_SIZE];
    let mut b = vec![0u8; SECTOR_SIZE];
    for &lba in &lbas {
        source.read_sector(lba, &mut a).unwrap();
        disk.read_sector(lba, &mut b).unwrap();
        assert_eq!(a, b, "lba {lba}");
    }
    // Padding, if any, reads as zero.
    for lba in source.total_sectors()..disk.total_sectors() {
        disk.read_sector(lba, &mut b).unwrap();
        assert!(b.iter().all(|&x| x == 0), "padding sector {lba}");
    }

    // A write goes to the sidecar, persists across a reopen, and leaves the
    // CHD byte-identical.
    let written = vec![0x5A; SECTOR_SIZE];
    disk.write_sector(1234, &written).unwrap();
    disk.flush().unwrap();
    drop(disk);
    assert_eq!(std::fs::read(&copy).unwrap(), pristine, "CHD untouched");
    assert!(
        overlay_path(&copy).is_file(),
        "sidecar created beside the copy"
    );
    let mut disk = open(&copy);
    disk.read_sector(1234, &mut b).unwrap();
    assert_eq!(b, written);
    disk.read_sector(1235, &mut b).unwrap();
    source.read_sector(1235, &mut a).unwrap();
    assert_eq!(a, b, "the neighbouring sector still comes from the CHD");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "runs the emulator and needs KICK31.ROM plus the AmigaSYS CHD (see tests/README.md)"]
fn chd_hard_disk_boots_amigasys_under_kick31() {
    let (Some(chd), Some(kick)) = (asset("AmigaSYS3PlusAGA-rdb.chd"), asset("KICK31.ROM")) else {
        eprintln!("skipping: AmigaSYS CHD or KICK31.ROM not in the asset directory");
        return;
    };
    let (dir, copy) = scratch_copy(&chd);
    let pristine_len = std::fs::metadata(&copy).unwrap().len();
    let cfg = dir.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"[machine]
profile = "A1200"

[memory]
fast = "8M"

[ide]
master = "{}"
"#,
            toml_path(&copy)
        ),
    )
    .unwrap();

    let png = dir.join("boot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("RUST_LOG", "copperline=info")
        .arg("--factory")
        .arg("--config")
        .arg(&cfg)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("90")
        .arg(&png)
        .arg(&kick)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "copperline failed: {}\n{log}",
        output.status
    );
    assert!(
        log.contains("is a CHD hard disk"),
        "the drive was not attached from the CHD:\n{log}"
    );
    // AmigaSYS's Workbench is a full-colour AGA desktop: far more than the
    // handful of colours a boot failure (insert-disk screen, guru) shows.
    let colors = distinct_colors(&png);
    assert!(
        colors > 64,
        "only {colors} distinct colours after boot; see {log}"
    );
    // The boot wrote to the disk (Workbench updates its volume state), all of
    // it in the sidecar.
    assert_eq!(std::fs::metadata(&copy).unwrap().len(), pristine_len);
    assert!(overlay_path(&copy).is_file());

    let _ = std::fs::remove_dir_all(&dir);
}
