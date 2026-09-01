// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end verification of copperhf.device's M4 command coverage
//! (`COPPERHF-DEVICE-PLAN.md`'s "M4 -- Command coverage"): a headless AROS
//! boot autoboots the DiagArea-resident device against a 2-unit config, the
//! guest probe (`guest/copperhf-test/chftest_m4`) `OpenDevice`s both units
//! and drives NSCMD_DEVICEQUERY, TD_CHANGENUM/CHANGESTATE/PROTSTATUS,
//! TD_READ64, HD_SCSICMD INQUIRY/READ CAPACITY(10), and the
//! TD_ADDCHANGEINT/TD_EJECT/TD_REMCHANGEINT change-interrupt story, then
//! this test verifies each subtest by reading its own marker straight back
//! out of unit 0's image file -- the same "no hostfs plumbing needed" trick
//! `tests/copperhf_device.rs`'s M2 test uses, extended to one marker block
//! per subtest so a failure names exactly which check failed.
//!
//! A second probe rather than an extension of `chftest.c` (M2): keeps that
//! test's own marker layout completely undisturbed. See
//! `guest/copperhf-test/chftest_m4.c`'s own header comment for the full
//! command-by-command rundown.
//!
//! Needs no local assets at all: the bundled AROS ROM boots the `--run`
//! staging volume, so this runs on any checkout (see
//! `tests/copperhf_device.rs`'s own header comment for the same
//! bundled-ROM `--run` pattern this borrows its emulated-time budget from).
//! Release-only for the same reason as every other AROS-boot integration
//! test in this suite: a debug-build emulator is far too slow for a full
//! AROS boot plus `--run` staging.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const SECTOR_SIZE: usize = 512;
// Block 0: the seeded i % 251 pattern TD_READ64 verifies against. Blocks
// 1-8: one marker per subtest (guest/copperhf-test/chftest_m4.c's BLK_*
// constants). A few spare sectors of headroom past that.
const IMAGE_SECTORS: usize = 16;
const UNIT1_SECTORS: usize = 8; // never read/written; just needs to exist.

struct Subtest {
    block: usize,
    name: &'static str,
    ok_marker: &'static str,
    bad_marker: &'static str,
}

// Mirrors guest/copperhf-test/chftest_m4.c's BLK_*/marker-string constants
// exactly -- keep the two in sync.
const SUBTESTS: &[Subtest] = &[
    Subtest {
        block: 1,
        name: "NSCMD_DEVICEQUERY",
        ok_marker: "M4-NSDQ-OK",
        bad_marker: "M4-NSDQ-BAD",
    },
    Subtest {
        block: 2,
        name: "TD_CHANGENUM",
        ok_marker: "M4-CHGNUM-OK",
        bad_marker: "M4-CHGNUM-BAD",
    },
    Subtest {
        block: 3,
        name: "TD_CHANGESTATE",
        ok_marker: "M4-CHGSTATE-OK",
        bad_marker: "M4-CHGSTATE-BAD",
    },
    Subtest {
        block: 4,
        name: "TD_PROTSTATUS",
        ok_marker: "M4-PROTSTAT-OK",
        bad_marker: "M4-PROTSTAT-BAD",
    },
    Subtest {
        block: 5,
        name: "TD_READ64",
        ok_marker: "M4-READ64-OK",
        bad_marker: "M4-READ64-BAD",
    },
    Subtest {
        block: 6,
        name: "HD_SCSICMD INQUIRY",
        ok_marker: "M4-INQUIRY-OK",
        bad_marker: "M4-INQUIRY-BAD",
    },
    Subtest {
        block: 7,
        name: "HD_SCSICMD READ CAPACITY(10)",
        ok_marker: "M4-READCAP-OK",
        bad_marker: "M4-READCAP-BAD",
    },
    Subtest {
        block: 8,
        name: "TD_ADDCHANGEINT/TD_EJECT/TD_REMCHANGEINT change-interrupt story",
        ok_marker: "M4-CHGINT-OK",
        bad_marker: "M4-CHGINT-BAD",
    },
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

/// A flat, non-"DOS"/non-"RDSK" image, same reasoning as
/// `tests/copperhf_device.rs::seeded_image`: keeps block 0 (and therefore
/// every subsequent block) a direct file offset with no host-side
/// synthesized-RDB shift, so the guest probe's raw byte offsets line up
/// with this test's own.
fn seeded_unit0_image(path: &Path) {
    let mut bytes = vec![0u8; IMAGE_SECTORS * SECTOR_SIZE];
    for (i, b) in bytes.iter_mut().enumerate().take(SECTOR_SIZE) {
        *b = (i % 251) as u8;
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}

/// Unit 1's image: only ever `TD_EJECT`ed, never read or written, so its
/// content doesn't matter -- zero-filled (also not a "DOS"/"RDSK" image, so
/// it mounts as a plain attached-but-unmounted unit like unit 0 would
/// without a filesystem).
fn blank_unit1_image(path: &Path) {
    let bytes = vec![0u8; UNIT1_SECTORS * SECTOR_SIZE];
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
fn m4_command_coverage_over_aros_autoboot() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping copperhf.device M4 end-to-end probe; run with --release \
             (a debug emulator is far too slow for a full AROS boot + --run staging)"
        );
        return Ok(());
    }

    let root = repo_root();
    let scratch = std::env::temp_dir().join(format!(
        "copperline-copperhf-m4-guest-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    let program_dir = scratch.join("program");
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&program_dir)?;
    std::fs::create_dir_all(&config_home)?;

    let probe = program_dir.join("chftest_m4");
    std::fs::copy(root.join("guest/copperhf-test/chftest_m4"), &probe)?;

    let unit0 = scratch.join("unit0.image");
    let unit1 = scratch.join("unit1.image");
    seeded_unit0_image(&unit0);
    blank_unit1_image(&unit1);

    let config = scratch.join("copperhf.toml");
    std::fs::write(
        &config,
        format!(
            "rom = \"<bundled-aros>\"\n\n[copperhf]\nunit0 = {}\nunit1 = {}\n",
            toml_path(&unit0),
            toml_path(&unit1),
        ),
    )?;

    let screenshot = scratch.join("copperhf-m4.png");
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
        // Same emulated-time budget as tests/copperhf_device.rs's M2 test:
        // bundled-AROS `--run` hands the guest control at ~11s emulated,
        // this probe does more work than M2's but is still a handful of
        // synchronous DoIO/SendIO round trips plus one softint-poll spin,
        // nowhere near 50s of emulated headroom.
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

    let bytes = std::fs::read(&unit0)?;
    let mut failures = Vec::new();
    let mut any_ran = false;

    for subtest in SUBTESTS {
        let start = subtest.block * SECTOR_SIZE;
        let marker = &bytes[start..start + SECTOR_SIZE];
        if marker.starts_with(subtest.ok_marker.as_bytes()) {
            any_ran = true;
        } else if marker.starts_with(subtest.bad_marker.as_bytes()) {
            any_ran = true;
            failures.push(format!("{} reported FAIL", subtest.name));
        } else {
            failures.push(format!(
                "{} marker absent (block {} read back {:?}) -- the probe never reached this \
                 subtest",
                subtest.name,
                subtest.block,
                &marker[..subtest.ok_marker.len().max(marker.len().min(32))],
            ));
        }
    }

    assert!(
        any_ran,
        "no M4 subtest marker found at all -- the guest probe never ran or \
         OpenDevice(\"copperhf.device\") failed on both units:\nstdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
    assert!(
        failures.is_empty(),
        "copperhf.device M4 guest probe reported failures:\n{}\nstdout:\n{}\nstderr:\n{}",
        failures.join("\n"),
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    std::fs::remove_dir_all(&scratch)?;
    Ok(())
}
