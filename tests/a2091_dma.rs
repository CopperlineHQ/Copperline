// SPDX-License-Identifier: GPL-3.0-or-later
//! Byte-accurate, guest-driven tests of the bundled A2091 ROM's DMA buffers.
//! The probe talks to scsi.device directly; disk images are private fixtures.

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dma_addresses(log: &str) -> Vec<u32> {
    let mut high = 0;
    let mut low = 0;
    let mut addresses = Vec::new();
    for line in log.lines() {
        for (register, value) in [("0x0084/2", &mut high), ("0x0086/2", &mut low)] {
            if line.contains(&format!("a2091 wr {register}")) {
                let hex = line
                    .split("<- 0x")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap();
                *value = u32::from_str_radix(hex, 16).unwrap();
            }
        }
        if line.contains("a2091 rd 0x00E0/2") {
            addresses.push((high << 16) | low);
        }
    }
    addresses
}

fn run_case(name: &str, kick: Option<&Path>, fast: &str, chip: &str) {
    run_case_with_lba(name, kick, fast, chip, false);
}

fn run_case_with_lba(name: &str, kick: Option<&Path>, fast: &str, chip: &str, boundary: bool) {
    let temp = std::env::temp_dir().join(format!(
        "copperline-a2091-dma-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp).unwrap();
    let exe = temp.join(if cfg!(windows) {
        "copperline.exe"
    } else {
        "copperline"
    });
    if !exe.exists() && std::fs::hard_link(env!("CARGO_BIN_EXE_copperline"), &exe).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_copperline"), &exe).unwrap();
    }
    // Portable mode keeps --run's staged boot directory inside this fixture.
    std::fs::write(temp.join("portable.txt"), "").unwrap();
    let probe = temp.join("dmatest");
    std::fs::copy(root().join("guest/a2091-test/dmatest"), &probe).unwrap();
    let disk = temp.join("disk.hdf");
    let initial: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    std::fs::write(&disk, &initial).unwrap();
    let large_disk = temp.join("large.hdf");
    if boundary {
        // Unix regular files support holes: logical size exceeds 2 TiB,
        // but this private fixture writes only the 128 KiB boundary window.
        let start = (1u64 << 41) - 64 * 1024;
        let data: Vec<u8> = (0..128 * 1024 + 512).map(|i| (i % 251) as u8).collect();
        let mut file = std::fs::File::create(&large_disk).unwrap();
        file.set_len(start + data.len() as u64).unwrap();
        file.seek(SeekFrom::Start(start)).unwrap();
        file.write_all(&data).unwrap();
        std::fs::write(temp.join("a2091-lba"), "").unwrap();
    }
    let config = temp.join("config.toml");
    let quoted = |p: &Path| {
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    };
    let boundary_config = if boundary {
        format!("unit1 = \"{}\"", quoted(&large_disk))
    } else {
        String::new()
    };
    let cpu = if kick.is_some() { "68020" } else { "68000" };
    let accelerator = if kick.is_some() { "8M" } else { "0" };
    std::fs::write(
        &config,
        format!(
            r#"
[machine]
profile = "A500"
[cpu]
model = "{cpu}"
[memory]
chip = "{chip}"
fast = "{fast}"
slow = "0"
accelerator = "{accelerator}"
[chipset]
revision = "ECS"
[scsi]
controller = "a2091"
rom = "{}"
unit0 = "{}"
{boundary_config}
"#,
            quoted(&root().join("assets/a2091/copperline-a2091.rom")),
            quoted(&disk)
        ),
    )
    .unwrap();
    let mut command = Command::new(exe);
    command
        .env("COPPERLINE_AROS_DIR", root().join("assets/aros"))
        .env("COPPERLINE_DIAG_A2091", "1")
        .env("RUST_LOG", "copperline=info")
        .args(["--factory", "--config"])
        .arg(config)
        .args(["--noaudio", "--run"])
        .arg(probe)
        .args(["--exit-on-return", "--screenshot-after", "90"])
        .arg(temp.join("last.png"));
    if let Some(rom) = kick {
        command.arg(rom);
    }
    let output = command.output().unwrap();
    let log = String::from_utf8_lossy(&output.stderr);
    std::fs::write(temp.join("run.log"), log.as_bytes()).unwrap();
    let report_path = temp.join("a2091-result");
    assert!(
        report_path.is_file(),
        "{name}: probe did not finish; inspect {}",
        temp.display()
    );
    let report = std::fs::read(report_path).unwrap();
    let words: Vec<u32> = report
        .chunks_exact(4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(words[0], 0x41323039);
    assert!(
        output.status.success(),
        "{name}: guest failure, report {words:x?}; inspect {}",
        temp.display()
    );
    assert_eq!(
        words[1],
        if boundary { 1023 } else { 511 },
        "{name}: a guest data/guard/error check failed"
    );
    assert!(
        if kick.is_some() {
            words[2] >= 0x01000000
        } else {
            words[2] & 1 != 0
        },
        "destination must require a bounce buffer"
    );
    assert_eq!(
        words[5],
        64 * 1024,
        "must preserve successful chunks on a later error"
    );
    if chip != "512K" {
        assert_eq!(words[3], words[6], "Chip RAM leak");
        assert_eq!(words[4], words[7], "Fast RAM leak");
    }
    let addresses = dma_addresses(&log);
    assert!(!addresses.is_empty(), "driver never started DMA");
    assert!(addresses.iter().all(|a| a & 1 == 0 && *a < 0x01000000));
    if fast != "0" {
        assert!(
            addresses
                .iter()
                .any(|a| (0x00200000..0x00a00000).contains(a)),
            "Fast RAM not used"
        );
    } else {
        assert!(
            addresses.iter().all(|a| *a < 0x00200000),
            "DMA must use Chip RAM without Z2 RAM"
        );
    }
    if chip != "512K" {
        assert!(
            addresses.windows(6).any(|a| {
                a[0] + 64 * 1024 == a[1]
                    && a[0] == a[2]
                    && a[0] == a[4]
                    && a[1] == a[3]
                    && a[1] == a[5]
            }),
            "{name}: read did not alternate two 64 KiB DMA buffers"
        );
    }
    let written = std::fs::read(&disk).unwrap();
    let offset = 1024 * 1024;
    let length = 320 * 1024 + 512;
    assert_eq!(
        &written[..offset],
        &initial[..offset],
        "write touched preceding sectors"
    );
    assert!(written[offset..offset + length]
        .iter()
        .enumerate()
        .all(|(i, b)| *b == (i as u8).wrapping_mul(13).wrapping_add(7)));
    assert_eq!(
        &written[offset + length..],
        &initial[offset + length..],
        "write touched following sectors"
    );
    std::fs::remove_dir_all(temp).unwrap();
}

#[test]
#[ignore = "runs the emulator"]
fn a2091_dma_aros_fast_and_chip_memory() {
    run_case("aros-fast", None, "2M", "2M");
    run_case("aros-chip", None, "0", "2M");
}

#[test]
#[ignore = "runs the emulator and requires local Kickstart ROMs"]
fn a2091_dma_kickstarts() {
    let assets = std::env::var_os("COPPERLINE_A2091_TEST_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("test-assets"));
    for (file, name, fast) in [
        ("KICK31.ROM", "kick31-512k", "512K"),
        ("KICK31.ROM", "kick31-1m", "1M"),
        ("KICK31.ROM", "kick31-2m", "2M"),
        ("KICK31.ROM", "kick31-scarce", "0"),
        ("KICK13.ROM", "kick13-fast", "1M"),
        ("KICK13.ROM", "kick13-chip", "0"),
    ] {
        let rom = assets.join(file);
        if !rom.is_file() {
            eprintln!("skipping {name}: missing {}", rom.display());
            continue;
        }
        run_case(
            name,
            Some(&rom),
            fast,
            if name == "kick31-scarce" {
                "512K"
            } else {
                "2M"
            },
        );
    }
}

#[cfg(unix)]
#[test]
#[ignore = "runs the emulator with a sparse disk larger than 2 TiB"]
fn a2091_dma_read_crosses_32_bit_lba_boundary() {
    run_case_with_lba("aros-lba", None, "2M", "2M", true);
}
