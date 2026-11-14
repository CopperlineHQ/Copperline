// SPDX-License-Identifier: GPL-3.0-or-later
//! The SCSI tape drive, driven by a guest through a real `scsi.device`.
//!
//! `guest/tape-test/tapetest` opens unit 4 and works the drive with SCSI
//! Direct the way tape software does (fixed- and variable-block writes and
//! reads, filemarks, SPACE, LOAD/UNLOAD, ERASE), checking every status and
//! the autosense data behind each exception. Its return code names the
//! first step that misbehaved; this test also reads what it left in the
//! cartridge directory, which is where a tape's files live on the host.
//!
//! The default run needs no assets: the bundled AROS ROM boots the probe
//! and the bundled A2091 ROM provides `scsi.device`. A second test repeats
//! it on an A3000 with Commodore's own driver when that Kickstart is
//! installed.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn quoted(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// The bytes `tapetest`'s `fill(seed, len)` writes.
fn pattern(seed: u8, len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| seed.wrapping_add((i as u8).wrapping_mul(7)))
        .collect()
}

fn run_probe(name: &str, machine: &str, rom: Option<&Path>) {
    let temp = std::env::temp_dir().join(format!(
        "copperline-scsi-tape-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(&temp).unwrap();
    let exe = temp.join(if cfg!(windows) {
        "copperline.exe"
    } else {
        "copperline"
    });
    if std::fs::hard_link(env!("CARGO_BIN_EXE_copperline"), &exe).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_copperline"), &exe).unwrap();
    }
    // Portable mode keeps --run's staged boot directory inside this fixture.
    std::fs::write(temp.join("portable.txt"), "").unwrap();
    let probe = temp.join("tapetest");
    std::fs::copy(root().join("guest/tape-test/tapetest"), &probe).unwrap();
    let cartridge = temp.join("cartridge");
    std::fs::create_dir_all(&cartridge).unwrap();
    let config = temp.join("config.toml");
    std::fs::write(
        &config,
        format!(
            "{machine}\n[[tape]]\nunit = 4\npath = \"{}\"\n",
            quoted(&cartridge)
        ),
    )
    .unwrap();

    let mut command = Command::new(exe);
    command
        .env("COPPERLINE_AROS_DIR", root().join("assets/aros"))
        .env("RUST_LOG", "copperline=info")
        .args(["--factory", "--config"])
        .arg(&config)
        .args(["--noaudio", "--run"])
        .arg(&probe)
        .args(["--exit-on-return", "--screenshot-after", "120"])
        .arg(temp.join("last.png"));
    if let Some(rom) = rom {
        command.arg(rom);
    }
    let output =
        common::output_with_timeout(&mut command, std::time::Duration::from_secs(600)).unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    std::fs::write(temp.join("run.log"), log.as_bytes()).unwrap();
    assert!(
        log.contains("unit 4 tape drive"),
        "{name}: the tape drive was not attached; inspect {}",
        temp.display()
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{name}: tapetest failed (100 + step, 97 = OpenDevice, 4 = never returned); \
         inspect {}",
        temp.display()
    );

    // Two tape files are left: two fixed 512-byte blocks, and one
    // 300-byte variable block. The third, written at end-of-data, was
    // erased again.
    let mut names: Vec<String> = std::fs::read_dir(&cartridge)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["00000.tape", "00001.tape"], "{name}");
    assert_eq!(
        std::fs::read(cartridge.join("00000.tape")).unwrap(),
        pattern(0x11, 1024),
        "{name}: tape file 0"
    );
    assert_eq!(
        std::fs::read(cartridge.join("00001.tape")).unwrap(),
        pattern(0x33, 300),
        "{name}: tape file 1"
    );
    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
#[cfg_attr(debug_assertions, ignore = "requires --profile ci or --release")]
fn a_guest_reads_and_writes_tape_files_through_the_a2091() {
    common::require_optimized();
    let rom = root().join("assets/a2091/copperline-a2091.rom");
    run_probe(
        "a2091",
        &format!(
            "[machine]\nprofile = \"A500\"\n[memory]\nchip = \"1M\"\nfast = \"2M\"\n\
             [chipset]\nrevision = \"ECS\"\n[scsi]\ncontroller = \"a2091\"\nrom = \"{}\"\n",
            quoted(&rom)
        ),
        None,
    );
}

/// Commodore's `scsi.device` on the A3000's own controller. Needs an A3000
/// Kickstart image in the test asset directory, and fails without one.
#[test]
#[ignore = "needs a local A3000 Kickstart"]
fn a_guest_reads_and_writes_tape_files_through_the_a3000_scsi_device() {
    common::require_optimized();
    let assets = std::env::var_os("COPPERLINE_TEST_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("test-assets"));
    let rom = [
        "kicka3000.rom",
        "Kickstart v3.1 r40.68 (1993)(Commodore)(A3000).rom",
    ]
    .iter()
    .map(|name| assets.join(name))
    .find(|path| path.is_file());
    let rom = rom.unwrap_or_else(|| {
        panic!(
            "this test needs an A3000 Kickstart (kicka3000.rom) in {}",
            assets.display()
        )
    });
    run_probe(
        "a3000",
        "[machine]\nprofile = \"A3000\"\n[scsi]\ncontroller = \"a3000\"\n",
        Some(&rom),
    );
}
