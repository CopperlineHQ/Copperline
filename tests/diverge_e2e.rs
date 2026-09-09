// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end check of `copperline-ctl diverge`: the built ctl binary
//! launches the built emulator twice with the bundled AROS ROM, so no
//! local assets are needed. Once against itself (no divergence) and once
//! with a config that changes the CPU model (a divergence at the first
//! frame, narrowed to the first instruction). Ignored like the rest of
//! this directory because it spawns processes and runs the emulator:
//!
//! ```sh
//! cargo test --release --test diverge_e2e -- --ignored
//! ```

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("copperline-diverge-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn run_diverge(args: &[&str]) -> (i32, Value, String) {
    let ctl = env!("CARGO_BIN_EXE_copperline-ctl");
    let emulator = env!("CARGO_BIN_EXE_copperline");
    let output = Command::new(ctl)
        .arg("diverge")
        .arg("--a")
        .arg(emulator)
        .arg("--b")
        .arg(emulator)
        .args(args)
        .output()
        .expect("running copperline-ctl diverge");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let json: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}\nstderr:\n{stderr}"));
    (output.status.code().unwrap_or(-1), json, stderr)
}

#[test]
#[ignore]
fn the_same_binary_and_config_never_diverge() {
    let (code, report, stderr) = run_diverge(&[
        "--frames",
        "40",
        "--stride",
        "10",
        "--memory",
        "all",
        "--json",
        "--",
        "--factory",
        "--model",
        "A500",
        "--noaudio",
    ]);
    assert_eq!(code, 0, "{report}\n{stderr}");
    assert_eq!(report["outcome"], "identical", "{report}");
    assert_eq!(report["frames_compared"], 40);
    assert_eq!(report["end_frame"], 40);
    assert_eq!(report["memory"], "all");
    assert!(report["divergence"].is_null());
    assert!(report["sides"][0]["emulator"]
        .as_str()
        .unwrap()
        .starts_with("copperline "));
    assert_eq!(
        report["sides"][0]["state_version"],
        report["sides"][1]["state_version"]
    );
    assert!(report["sides"][0]["state_version"].is_u64());
}

#[test]
#[ignore]
fn a_differing_cpu_config_diverges_at_the_first_frame() {
    let config_a = scratch("a.toml");
    let config_b = scratch("b.toml");
    std::fs::write(&config_a, "[cpu]\nmodel = \"68000\"\n").unwrap();
    std::fs::write(&config_b, "[cpu]\nmodel = \"68020\"\n").unwrap();
    let shots = scratch("shots");
    let (code, report, stderr) = run_diverge(&[
        "--config-a",
        config_a.to_str().unwrap(),
        "--config-b",
        config_b.to_str().unwrap(),
        "--frames",
        "40",
        "--stride",
        "10",
        "--screenshots",
        shots.to_str().unwrap(),
        "--json",
        "--",
        "--factory",
        "--model",
        "A500",
        "--noaudio",
    ]);
    assert_eq!(code, 1, "{report}\n{stderr}");
    assert_eq!(report["outcome"], "diverged", "{report}");
    let d = &report["divergence"];
    assert_eq!(d["frame"], 1, "{d}");
    assert_eq!(d["last_matching_frame"], 0);
    assert_eq!(d["at_start"], false);
    // A different CPU retires a different instruction stream inside the
    // frame, so the narrowing lands on a CPU-visible difference.
    let cpu = &d["cpu"];
    assert!(!cpu.is_null(), "{d}");
    assert!(cpu["step"].as_u64().unwrap() >= 1);
    assert!(matches!(d["kind"].as_str(), Some("cpu" | "timing")), "{d}");
    assert_eq!(d["dma_only"], false);
    assert_eq!(d["step_cap_reached"], false);
    let a = PathBuf::from(d["screenshots"][0].as_str().unwrap());
    let b = PathBuf::from(d["screenshots"][1].as_str().unwrap());
    assert!(a.is_file() && b.is_file(), "{d}");
    assert!(a.ends_with("a-frame-1.png") && b.ends_with("b-frame-1.png"));
    std::fs::remove_dir_all(shots.parent().unwrap()).ok();
}
