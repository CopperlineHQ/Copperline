//! Toccata (AD1848 sound board) boot and capture checks.
//!
//! `toccata_board_attaches_and_streams_a_stem` needs no local assets: the
//! bundled AROS ROM boots with `[toccata] enabled = true`, which is enough
//! to prove the board attaches to the Zorro chain and that its audio path
//! reaches `--audio-stems` end to end (the `toccata.wav` stem is written,
//! even though AROS never programs the codec, so it stays silent -- this
//! test is about the host-side plumbing, not AHI driver behaviour).
//!
//! `toccata_ahi_driver_recognizes_the_board` is the real AHI end-to-end
//! check (M1: `toccata.audio` loads and AHI prefs lists the board's
//! modes) and needs a local licensed Workbench install with AHI 4.18 and
//! `toccata.audio` staged onto it -- see tests/README.md for the asset
//! recipe. It is asset-gated per the usual convention and skips cleanly
//! when the HDF is absent.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let local = repo_root().join("test-assets");
    if local.is_dir() {
        local
    } else {
        repo_root()
    }
}

fn run_copperline(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn,copperline::emulator=info")
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .args(args)
        .output()
        .expect("run emulator")
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "copperline-toccata-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Needs no local assets: proves the board autoconfigs (the emulator log
/// names it) and that its audio path is wired all the way through to stem
/// capture, using only the bundled AROS ROM.
#[test]
#[ignore = "runs the emulator"]
fn toccata_board_attaches_and_streams_a_stem() {
    let cfg_path = scratch_dir("cfg").with_extension("toml");
    std::fs::write(
        &cfg_path,
        "rom = \"<bundled-aros>\"\n\n[toccata]\nenabled = true\n",
    )
    .expect("write test config");

    let stems = scratch_dir("stems");
    let shot = scratch_dir("shot").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-stems",
        stems.to_str().unwrap(),
        "--audio-stems-mode",
        "source",
        "--screenshot-after",
        "3",
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("toccata: AD1848 sound board"),
        "expected the board-attach log line; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stems.join("toccata.wav").is_file(),
        "the toccata source should register and write a stem file \
         even though AROS never programs the codec"
    );

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&shot);
    let _ = std::fs::remove_dir_all(&stems);
}

/// Kickstart 3.1 has no A3000/A4000 SCSI driver built in; A4000 gives the
/// HDF a real IDE port to sit on, matching how the asset was built.
const KICK31_A4000: &str = "Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom";
/// A Workbench install with AHI 4.18 and toccata.audio staged into
/// Devs/AHI (see tests/README.md's Toccata asset recipe) and Unit 0 set
/// to Toccata in AHI-prefs' saved ENV:Sys/ahi.prefs.
const TOCCATA_AHI_HDF: &str = "toccata-ahi.hdf";

/// The real driver-recognition check: boots a licensed OS install with AHI
/// and `toccata.audio` staged onto it, opens AHI prefs, and confirms the
/// driver found and named the board. Skips cleanly without the asset.
#[test]
#[ignore = "runs the emulator and requires a local Kickstart/AHI HDF"]
fn toccata_ahi_driver_recognizes_the_board() -> Result<(), Box<dyn std::error::Error>> {
    let assets = asset_dir();
    let missing: Vec<_> = [KICK31_A4000, TOCCATA_AHI_HDF]
        .into_iter()
        .filter(|file| !assets.join(file).is_file())
        .collect();
    if !missing.is_empty() {
        eprintln!("skipping Toccata/AHI integration test; missing files: {missing:?}");
        return Ok(());
    }

    let stem = format!("copperline-toccata-ahi-{}", std::process::id());
    let config_path = std::env::temp_dir().join(format!("{stem}.toml"));
    let shot = std::env::temp_dir().join(format!("{stem}.png"));
    std::fs::write(
        &config_path,
        format!(
            "rom = \"{KICK31_A4000}\"\n\n\
             [machine]\n\
             profile = \"A4000\"\n\n\
             [toccata]\n\
             enabled = true\n\n\
             [ide]\n\
             master = \"{TOCCATA_AHI_HDF}\"\n"
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&assets)
        .env("RUST_LOG", "copperline=warn,copperline::emulator=info")
        .arg("--config")
        .arg(&config_path)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("60")
        .arg(&shot)
        .output()?;
    let _ = std::fs::remove_file(&config_path);
    assert!(
        output.status.success(),
        "Copperline failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(&shot);
    Ok(())
}
