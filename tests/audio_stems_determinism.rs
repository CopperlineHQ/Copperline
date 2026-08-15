//! `--audio-stems` regression coverage: the file set matches the selected
//! granularity, an omitted `--audio-stems-mode` (with no config default) is
//! a usage error, and two runs of the same scenario produce byte-identical
//! stem files -- the whole point of driving capture purely from emulated
//! time (see docs/internals/audio.md).
//!
//! Needs no local assets: the bundled AROS ROM boots with an empty DF0
//! (present by default, no image inserted), which is enough to exercise
//! `drivesounds` via the empty-drive change-line poll -- see
//! `docs/guide/configuration.md`'s `[audio]` section for why that's audible
//! with no disk at all.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A minimal config: bundled AROS, default (empty) DF0, default `[audio]`.
/// No CD drive or MT-32 configured, so `configured_audio_stem_sources`
/// registers only `paula` and `drivesounds`.
fn write_minimal_config(path: &Path) {
    std::fs::write(path, "rom = \"<bundled-aros>\"\n").expect("write test config");
}

fn run_copperline(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .args(args)
        .output()
        .expect("run emulator")
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read stems dir {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "copperline-audio-stems-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// `master` alone writes exactly `master.wav`; `source,channel` together
/// write every file the unconfigured-CD/MT-32 source set implies (Paula's
/// sum, its four channels, and drive sounds), and nothing else -- no
/// `cdda.wav`/`mt32.wav` shows up just because neither is configured.
#[test]
#[ignore = "runs the emulator"]
fn stems_file_set_matches_the_selected_granularity() {
    let cfg_path = scratch_dir("cfg").with_extension("toml");
    write_minimal_config(&cfg_path);

    let master_only = scratch_dir("master-only");
    let shot = scratch_dir("shot-a").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-stems",
        master_only.to_str().unwrap(),
        "--audio-stems-mode",
        "master",
        "--screenshot-after",
        "3",
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "master-only run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(file_names(&master_only), vec!["master.wav"]);

    let full = scratch_dir("source-channel");
    let shot2 = scratch_dir("shot-b").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-stems",
        full.to_str().unwrap(),
        "--audio-stems-mode",
        "source,channel",
        "--screenshot-after",
        "3",
        shot2.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "source+channel run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        file_names(&full),
        vec![
            "drivesounds.wav",
            "paula-0.wav",
            "paula-1.wav",
            "paula-2.wav",
            "paula-3.wav",
            "paula.wav",
        ],
        "no cdda.wav/mt32.wav should appear when neither is configured"
    );

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_dir_all(&master_only);
    let _ = std::fs::remove_dir_all(&full);
    let _ = std::fs::remove_file(&shot);
    let _ = std::fs::remove_file(&shot2);
}

/// `--audio-stems` with no `--audio-stems-mode` and no `[audio]
/// stem_granularity` default is a usage error, not a silent default
/// granularity -- capture behavior must be explicit.
#[test]
#[ignore = "runs the emulator"]
fn audio_stems_without_a_mode_or_config_default_is_a_usage_error() {
    let cfg_path = scratch_dir("cfg-nomode").with_extension("toml");
    write_minimal_config(&cfg_path);
    let dir = scratch_dir("nomode");
    let shot = scratch_dir("shot-nomode").with_extension("png");

    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-stems",
        dir.to_str().unwrap(),
        "--screenshot-after",
        "3",
        shot.to_str().unwrap(),
    ]);
    assert!(
        !out.status.success(),
        "expected a usage error with no --audio-stems-mode"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--audio-stems-mode"),
        "stderr should name the missing flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !dir.exists(),
        "no directory should be created on a usage error"
    );

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&shot);
}

/// Capture is driven purely by emulated time, so two runs of the same
/// scenario -- same config, same schedule -- must produce byte-identical
/// stem files, in warp (unpaced headless capture) as anywhere else. This is
/// what makes stem captures usable as golden files.
#[test]
#[ignore = "runs the emulator"]
fn stems_capture_is_byte_identical_across_two_runs() {
    let cfg_path = scratch_dir("cfg-det").with_extension("toml");
    write_minimal_config(&cfg_path);

    let run = |label: &str| {
        let dir = scratch_dir(label);
        let shot = scratch_dir(label).with_extension("png");
        let out = run_copperline(&[
            "--config",
            cfg_path.to_str().unwrap(),
            "--noaudio",
            "--audio-stems",
            dir.to_str().unwrap(),
            "--audio-stems-mode",
            "master,source,channel",
            "--screenshot-after",
            "3",
            shot.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "{label} run failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_file(&shot);
        dir
    };

    let dir_a = run("det-a");
    let dir_b = run("det-b");

    let names_a = file_names(&dir_a);
    let names_b = file_names(&dir_b);
    assert_eq!(names_a, names_b, "the two runs wrote different file sets");

    for name in &names_a {
        let bytes_a = std::fs::read(dir_a.join(name)).unwrap();
        let bytes_b = std::fs::read(dir_b.join(name)).unwrap();
        assert_eq!(
            bytes_a, bytes_b,
            "{name} differs between two runs of the same scenario"
        );
    }

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}
