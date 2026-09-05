// SPDX-License-Identifier: GPL-3.0-or-later
//! Run the staged boot commands on real ROMs and AROS. The probe records
//! CLI state through a relative file and returns 20; Echo must still run.
//! cargo test --release --test run_boot -- --ignored

use copperline::runprog::{prepare_with_options, RunOptions};
use std::path::PathBuf;
use std::process::Command;

fn boot(tag: &str, rom: Option<&str>, model: &str, detach: bool) {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rom = rom.map(|name| {
        std::env::var_os("COPPERLINE_TEST_ASSETS")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo.join("test-assets"))
            .join(name)
    });
    if rom.as_ref().is_some_and(|path| !path.is_file()) {
        eprintln!("skipping {tag}: missing {}", rom.unwrap().display());
        return;
    }
    let scratch = std::env::temp_dir().join(format!("copperline-run-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let program_dir = scratch.join("program");
    std::fs::create_dir_all(&program_dir).unwrap();
    let program = program_dir.join("probe with spaces");
    std::fs::copy(repo.join("guest/run-tools/probe"), &program).unwrap();
    let _ = std::fs::remove_file(program_dir.join("FROM-GUEST"));
    let prepared = prepare_with_options(
        &program,
        Some("\"quoted value\" 123"),
        RunOptions {
            stack: Some(32768),
            detach,
        },
        Some(&scratch.join("stage")),
    )
    .unwrap();
    if !detach {
        // Exercise the external helpers even on ROMs with internal commands.
        // Failed updates must preserve the valid threshold, stack, and lock
        // that the probe observes below.
        let script_path = prepared.boot_dir.join("S/Startup-Sequence");
        let script = std::fs::read_to_string(&script_path).unwrap();
        let mut checks = String::new();
        for (i, (tool, args)) in [
            ("FailAt", "2147483648"),
            ("FailAt", "-1"),
            ("FailAt", "20 21"),
            ("Stack", "2047"),
            ("Stack", "4294967296"),
            ("CD", "\"RunProg:probe with spaces\""),
            ("CD", "\"RunProg:missing directory\""),
            ("Echo", "\"one\" \"two\""),
        ]
        .iter()
        .enumerate()
        {
            checks.push_str(&format!(
                "RunBoot:C/{tool} >\"RunBoot:invalid-{i}\" {args}\n"
            ));
        }
        checks.push_str("RunBoot:C/Echo >\"RunBoot:escaped\" \"star** quote*\" line*Nend\"\n");
        std::fs::write(
            script_path,
            script.replace(
                "\"RunProg:probe with spaces\" \"quoted value\"",
                &format!("{checks}\"RunProg:probe with spaces\" \"quoted value\""),
            ),
        )
        .unwrap();
    }
    // Explicit mounts use the production staging code while keeping every
    // output in the test directory, without changing the user's HOME.
    let mut config = copperline::config::RawConfig::default();
    copperline::runprog::apply_to_raw(&mut config, &prepared);
    let config_path = scratch.join("machine.toml");
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_copperline"));
    command
        .args(["--config"])
        .arg(config_path)
        .args(["--model", model, "--noaudio"])
        .args(["--screenshot-after", "25"])
        .arg(scratch.join("screen.png"));
    if let Some(rom) = rom {
        command.arg(rom);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let data = std::fs::read(program_dir.join("FROM-GUEST")).unwrap_or_else(|e| {
        panic!(
            "{tag}: probe did not write: {e}; screenshot: {}",
            scratch.display()
        )
    });
    let word = |i: usize| u32::from_be_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!(word(0), 21, "FailAt");
    assert_eq!(word(1), 32768, "Stack CLI setting");
    // Some shells reserve space for their launch frame before publishing
    // the usable byte count at entry (AROS reserves 96 bytes).
    assert!(word(2) >= 32768 - 128, "usable program stack: {}", word(2));
    if detach {
        assert_ne!(word(3), 0, "background CLI");
        assert_eq!(word(4), 0, "detached console");
    }
    assert_eq!(&data[20..], b"\"quoted value\" 123\n", "CLI arguments");
    assert_eq!(
        std::fs::read(prepared.boot_dir.join("done")).unwrap(),
        b"done\n",
        "Echo must run even after the probe returns 20"
    );
    if !detach {
        for i in 0..8 {
            let error =
                std::fs::read_to_string(prepared.boot_dir.join(format!("invalid-{i}"))).unwrap();
            assert!(
                error.contains("Copperline boot command failed"),
                "invalid-{i}: {error}"
            );
        }
        assert_eq!(
            std::fs::read(prepared.boot_dir.join("escaped")).unwrap(),
            b"star* quote\" line\nend\n"
        );
    }
    std::fs::remove_dir_all(scratch).unwrap();
}

#[test]
#[ignore = "boots local Kickstart 1.3"]
fn kick13_foreground() {
    boot("kick13-foreground", Some("KICK13.ROM"), "A500", false);
}

#[test]
#[ignore = "boots local Kickstart 3.1"]
fn kick31_detached() {
    boot("kick31-detached", Some("KICK31.ROM"), "A1200", true);
}

#[test]
#[ignore = "boots bundled AROS"]
fn aros_foreground() {
    boot("aros-foreground", None, "A500", false);
}

#[test]
#[ignore = "boots bundled AROS"]
fn aros_detached() {
    boot("aros-detached", None, "A500", true);
}
