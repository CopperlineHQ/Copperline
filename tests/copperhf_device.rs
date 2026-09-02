// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end verification of copperhf.device's M2 boot ROM: a headless AROS
//! boot autoboots the DiagArea-resident device, the guest probe
//! (`guest/copperhf-test/chftest`) `OpenDevice`s it and drives a
//! `CMD_READ`/`CMD_WRITE`/`CMD_UPDATE` round trip against unit 0, and this
//! test verifies the result by reading the marker the probe writes straight
//! back out of the drive image file -- no hostfs output plumbing needed,
//! since the marker lives in the very image this test already created.
//!
//! Needs no local assets at all: the bundled AROS ROM boots the `--run`
//! staging volume, so this runs on any checkout (see
//! `tests/image_regression.rs::run_flag_boots_and_runs_a_guest_binary` for
//! the same bundled-ROM `--run` pattern this borrows its emulated-time
//! budget from). A debug-build emulator is far too slow for a full AROS
//! boot plus `--run` staging, so like `tests/probe_golden.rs` this test
//! skips itself under a debug build rather than `#[ignore]`ing outright --
//! it still runs by default under `cargo test --release`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const SECTOR_SIZE: usize = 512;
const IMAGE_SECTORS: usize = 8; // 4096 bytes: block 0 (pattern) + block 1 (marker) + headroom.
const OK_MARKER: &[u8] = b"COPPERHF-TEST-OK";
const BAD_MARKER: &[u8] = b"COPPERHF-TEST-BAD";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

/// A flat, non-"DOS"/non-"RDSK" image: the shared harddrive layer
/// (`HardDriveImage::open`) only wraps a "bare partition hardfile" (a boot
/// block starting `DOS`, no `RDSK` in the first 16 sectors) in a
/// synthesized RDB, which would shift every LBA by one virtual cylinder.
/// Starting the image with neither keeps block 0 a direct file offset --
/// the same fixture `src/copperhf.rs`'s own unit tests use -- so the guest
/// probe's raw `CMD_READ`/`CMD_WRITE` addresses line up with this test's
/// own byte offsets into the file with no mounter (M3) involved.
fn seeded_image(path: &Path) {
    let mut bytes = vec![0u8; IMAGE_SECTORS * SECTOR_SIZE];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

fn toml_path(path: &Path) -> String {
    // Single-quoted TOML literal string: no escape processing, so a Windows
    // temp path's backslashes survive untouched.
    format!("'{}'", path.display())
}

#[test]
fn opendevice_and_cmd_read_write_round_trip_over_aros_autoboot(
) -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping copperhf.device end-to-end probe; run with --release \
             (a debug emulator is far too slow for a full AROS boot + --run staging)"
        );
        return Ok(());
    }

    let root = repo_root();
    let scratch =
        std::env::temp_dir().join(format!("copperline-copperhf-device-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let program_dir = scratch.join("program");
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&program_dir)?;
    std::fs::create_dir_all(&config_home)?;

    let probe = program_dir.join("chftest");
    std::fs::copy(root.join("guest/copperhf-test/chftest"), &probe)?;

    let image = scratch.join("unit0.image");
    seeded_image(&image);

    let config = scratch.join("copperhf.toml");
    std::fs::write(
        &config,
        format!(
            "rom = \"<bundled-aros>\"\n\n[copperhf]\nunit0 = {}\n",
            toml_path(&image)
        ),
    )?;

    let screenshot = scratch.join("copperhf.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&root)
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--run")
        .arg(&probe)
        // Bundled-AROS `--run` boots hand the guest control at ~11s emulated
        // and a quick probe like this one finishes moments later; 50s
        // matches `run_flag_boots_and_runs_a_guest_binary`'s own budget for
        // the same bundled-ROM `--run` shape, with headroom to spare.
        .arg("--screenshot-after")
        .arg("50")
        .arg(&screenshot)
        .output()?;
    assert!(
        output.status.success(),
        "Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    let bytes = std::fs::read(&image)?;
    let marker = &bytes[SECTOR_SIZE..SECTOR_SIZE + SECTOR_SIZE];

    if marker.starts_with(BAD_MARKER) {
        panic!(
            "guest probe ran and reported CMD_READ verify failure (unit 0 block 0 did not \
             match the seeded i % 251 pattern):\nstdout:\n{}\nstderr:\n{}",
            tail(&output.stdout, 40),
            tail(&output.stderr, 80),
        );
    }
    assert!(
        marker.starts_with(OK_MARKER),
        "success marker absent from unit 0 block 1 -- the guest probe never ran, \
         OpenDevice(\"copperhf.device\") failed, or CMD_WRITE/CMD_UPDATE never landed \
         (block 1 read back {:?}):\nstdout:\n{}\nstderr:\n{}",
        &marker[..OK_MARKER.len().max(marker.len().min(32))],
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    std::fs::remove_dir_all(&scratch)?;
    Ok(())
}
