// SPDX-License-Identifier: GPL-3.0-or-later
//! Guest-measured hard-drive timing: `guest/hdbench` times sequential reads,
//! random reads and a write through the machine's own disk driver, and the
//! results must land where each drive profile's mechanism puts them (and,
//! for the A4000's Seagate ST3144A, near the period ScsiSpeed figures in
//! `HDD-TIMING-PLAN.md`). Windows are generous: the drivers' own CPU cost
//! is part of every figure.
//!
//! cargo test --release --test hd_timing -- --ignored --nocapture

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn asset_dir() -> PathBuf {
    std::env::var_os("COPPERLINE_TEST_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root().join("test-assets"))
}

/// What `hdbench` measured, as rates.
#[derive(Debug, PartialEq)]
struct Bench {
    words: Vec<u32>,
    /// Sequential read rate in KB/s (1000 bytes) at 512 B, 4 KiB, 32 KiB and
    /// 256 KiB per request.
    seq: [f64; 4],
    /// Mean milliseconds per random 4 KiB read.
    random_ms: f64,
    /// Write rate in KB/s.
    write: f64,
}

/// Run `hdbench` against `device` unit 0 on a fresh 16 MiB disk. `config`
/// gets the disk path and returns the machine's TOML; `rom` is the
/// Kickstart, or `None` for the bundled AROS.
fn bench(tag: &str, device: &str, rom: Option<&Path>, config: impl Fn(&str) -> String) -> Bench {
    let temp =
        std::env::temp_dir().join(format!("copperline-hd-timing-{tag}-{}", std::process::id()));
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
    std::fs::copy(root().join("guest/hdbench/hdbench"), temp.join("hdbench")).unwrap();
    std::fs::write(temp.join("hdbench-config"), format!("{device} 0")).unwrap();
    let disk = temp.join("disk.hdf");
    std::fs::write(&disk, vec![0u8; 16 * 1024 * 1024]).unwrap();
    let quoted = disk
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let config_path = temp.join("config.toml");
    std::fs::write(&config_path, config(&quoted)).unwrap();
    let mut command = Command::new(&exe);
    command
        .current_dir(&temp)
        .env("COPPERLINE_AROS_DIR", root().join("assets/aros"))
        .args(["--factory", "--config"])
        .arg(&config_path)
        .args(["--noaudio", "--run"])
        .arg(temp.join("hdbench"))
        .args(["--exit-on-return", "--screenshot-after", "150"])
        .arg(temp.join("last.png"));
    if let Some(rom) = rom {
        command.arg(rom);
    }
    let output = command.output().unwrap();
    std::fs::write(temp.join("run.log"), &output.stderr).unwrap();
    let report = std::fs::read(temp.join("hdbench-result"))
        .unwrap_or_else(|_| panic!("{tag}: hdbench did not finish; inspect {}", temp.display()));
    let words: Vec<u32> = report
        .chunks_exact(4)
        .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        .collect();
    assert_eq!(words[0], 0x4844_424E, "{tag}: not an hdbench report");
    assert!(
        output.status.success() && words[1] == 127,
        "{tag}: a benchmark step failed (mask {}); inspect {}",
        words[1],
        temp.display()
    );
    let hz = f64::from(words[2]);
    let rate = |ticks: u32, bytes: f64| bytes / (f64::from(ticks) / hz) / 1000.0;
    let bench = Bench {
        seq: [4, 5, 6, 7].map(|i| rate(words[i], 1024.0 * 1024.0)),
        random_ms: f64::from(words[8]) / hz / 32.0 * 1000.0,
        write: rate(words[9], 256.0 * 1024.0),
        words,
    };
    eprintln!("{tag}: {bench:?}");
    std::fs::remove_dir_all(&temp).unwrap();
    bench
}

fn a2091(timing: &str) -> impl Fn(&str) -> String + '_ {
    move |disk| {
        // The fixture runs a copy of the binary, so name the bundled ROM.
        let rom = root()
            .join("assets/a2091/copperline-a2091.rom")
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        format!(
            r#"
[machine]
profile = "A500"
[memory]
fast = "2M"
slow = "0"
[scsi]
controller = "a2091"
rom = "{rom}"
timing = "{timing}"
unit0 = "{disk}"
"#
        )
    }
}

fn within(tag: &str, what: &str, value: f64, range: std::ops::RangeInclusive<f64>) {
    assert!(
        range.contains(&value),
        "{tag}: {what} = {value:.1}, expected {range:?}"
    );
}

#[test]
#[ignore = "runs the emulator"]
fn a2091_drives_run_at_their_mechanisms_pace() {
    let instant = bench("a2091-instant", "scsi.device", None, a2091("instant"));
    within(
        "instant",
        "256 KiB reads KB/s",
        instant.seq[3],
        5000.0..=f64::MAX,
    );
    within("instant", "random 4 KiB ms", instant.random_ms, 0.0..=10.0);

    // ProDrive 80S: 875-1094 KB/s off the media, 64 KB DisCache.
    let prodrive = bench(
        "a2091-prodrive",
        "scsi.device",
        None,
        a2091("quantum-prodrive-80s"),
    );
    within(
        "prodrive",
        "256 KiB reads KB/s",
        prodrive.seq[3],
        700.0..=1094.0,
    );
    within(
        "prodrive",
        "512 B reads KB/s",
        prodrive.seq[0],
        50.0..=200.0,
    );
    within(
        "prodrive",
        "random 4 KiB ms",
        prodrive.random_ms,
        20.0..=45.0,
    );
    within("prodrive", "writes KB/s", prodrive.write, 400.0..=1094.0);

    // ST125N: 799 KB/s, no buffer, so a single-sector request waits for its
    // sector to come round every time (about one revolution, 16.7 ms).
    let st125n = bench("a2091-st125n", "scsi.device", None, a2091("seagate-st125n"));
    within("st125n", "256 KiB reads KB/s", st125n.seq[3], 450.0..=799.0);
    within("st125n", "512 B reads KB/s", st125n.seq[0], 20.0..=45.0);
    within("st125n", "random 4 KiB ms", st125n.random_ms, 40.0..=90.0);

    // Deterministic: the same machine measures the same ticks again. The
    // instant drive is the sharper check: no media timing absorbs a shift
    // in when the guest issues its requests, so any host state reaching the
    // guest's timers (a volume date taken from the host clock, once) shows
    // in its figures first.
    let again = bench(
        "a2091-prodrive-again",
        "scsi.device",
        None,
        a2091("quantum-prodrive-80s"),
    );
    assert_eq!(
        again.words, prodrive.words,
        "timed runs must repeat exactly"
    );
    let instant_again = bench("a2091-instant-again", "scsi.device", None, a2091("instant"));
    assert_eq!(
        instant_again.words, instant.words,
        "timed runs must repeat exactly"
    );
}

#[test]
#[ignore = "runs the emulator and requires local Kickstart ROMs"]
fn kickstart_ide_drives_run_at_their_mechanisms_pace() {
    let a1200 = asset_dir().join("Kickstart v3.1 r40.68 (1993)(Commodore)(A1200)[!].rom");
    if a1200.is_file() {
        let machine = |timing: &'static str| {
            move |disk: &str| {
                format!(
                    "[machine]\nprofile = \"A1200\"\n[ide]\ntiming = \"{timing}\"\nmaster = \"{disk}\"\n"
                )
            }
        };
        let instant = bench(
            "a1200-instant",
            "scsi.device",
            Some(&a1200),
            machine("instant"),
        );
        within(
            "a1200 instant",
            "256 KiB reads KB/s",
            instant.seq[3],
            2000.0..=f64::MAX,
        );
        // CP2064: 1130 KB/s off the media, 929 KB/s with its skews.
        let conner = bench(
            "a1200-cp2064",
            "scsi.device",
            Some(&a1200),
            machine("conner-cp2064"),
        );
        within(
            "a1200 cp2064",
            "256 KiB reads KB/s",
            conner.seq[3],
            750.0..=1130.0,
        );
        within(
            "a1200 cp2064",
            "random 4 KiB ms",
            conner.random_ms,
            15.0..=40.0,
        );
    } else {
        eprintln!("skipping the A1200 case: missing {}", a1200.display());
    }

    let a4000 = asset_dir().join("Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom");
    if a4000.is_file() {
        // The period A4000 ScsiSpeed run on an ST3144A measured 198, 804
        // and 1030 KB/s at 4 KiB, 32 KiB and 256 KiB: each multi-sector
        // request losing most of a revolution is what puts 4 KiB so low.
        let st3144a = bench("a4000-st3144a", "scsi.device", Some(&a4000), |disk| {
            format!(
                "[machine]\nprofile = \"A4000\"\n[ide]\ntiming = \"period\"\nmaster = \"{disk}\"\n"
            )
        });
        within(
            "a4000 st3144a",
            "4 KiB reads KB/s",
            st3144a.seq[1],
            150.0..=250.0,
        );
        within(
            "a4000 st3144a",
            "32 KiB reads KB/s",
            st3144a.seq[2],
            500.0..=900.0,
        );
        within(
            "a4000 st3144a",
            "256 KiB reads KB/s",
            st3144a.seq[3],
            750.0..=1178.0,
        );
    } else {
        eprintln!("skipping the A4000 case: missing {}", a4000.display());
    }
}
