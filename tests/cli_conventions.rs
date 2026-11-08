// SPDX-License-Identifier: GPL-3.0-or-later

//! Startup-only process tests: no emulator execution, assets, or display.
mod common;

use std::process::{Command, Output};
use std::time::Duration;

fn run(args: &[&str], desktop: bool) -> Output {
    let cwd = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_copperline"));
    cmd.args(args)
        .current_dir(cwd.path())
        .env("RUST_LOG", "off")
        .env("COPPERLINE_DESKTOP_LAUNCH", if desktop { "1" } else { "0" });
    common::output_with_timeout(&mut cmd, Duration::from_secs(15)).unwrap()
}

#[test]
fn help_and_version_use_stdout_and_exit_successfully() {
    for desktop in [false, true] {
        for flag in ["--help", "-h", "--version", "-V"] {
            let output = run(&[flag], desktop);
            assert!(output.status.success(), "{output:?}");
            assert!(output.stderr.is_empty(), "{output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            if matches!(flag, "--help" | "-h") {
                assert!(text.contains("Usage: copperline"), "{text}");
            } else {
                assert_eq!(
                    text.trim(),
                    concat!("copperline ", env!("CARGO_PKG_VERSION"))
                );
            }
        }
    }
}

#[test]
fn usage_errors_stay_on_stderr_without_a_dialog() {
    for args in [
        vec!["--modle=A1200"],
        vec!["--desktop", "--modle=A1200"],
        vec!["--help=true"],
        vec!["--version="],
    ] {
        let output = run(&args, true);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.starts_with("Error:"), "{error}");
        if args.iter().any(|arg| arg.starts_with("--modle")) {
            assert!(error.contains("did you mean \"--model\"?"), "{error}");
        }
    }
}

#[test]
fn desktop_markers_do_not_make_headless_config_errors_modal() {
    for (mode, values) in [
        ("--screenshot-after=0", vec!["out.png"]),
        ("--benchmark-until=1", vec![]),
        ("--coverage=out.info", vec!["--run=missing-program"]),
        ("--control=:0", vec![]),
        ("--gdb=:0", vec![]),
    ] {
        let mut args = vec!["--desktop", "--factory", "--noaudio", mode];
        args.extend(values);
        args.push("--config=missing-config.toml");
        let output = run(&args, true);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("missing-config.toml"), "{error}");
    }
}
