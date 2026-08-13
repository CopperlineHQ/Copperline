// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end WHDLoad boot regression: stage the committed `TestGame.lha`
//! fixture (tests/assets/whdload/) with the real WHDLoad support archives,
//! boot it in the built emulator, and assert the slave painted its teal
//! frame -- proving archive extraction, slave parsing, Kickstart
//! identification, boot-volume staging, the hostfs boot, and WHDLoad itself
//! handing control to the slave.
//!
//! Ignored under a plain `cargo test`; it skips cleanly (passing) when the
//! support archives (`tools/fetch-whdload.sh`) or a local Kickstart 3.1
//! (40.068 A1200) image are absent. See tests/README.md for the asset
//! contract.

use std::path::{Path, PathBuf};
use std::process::Command;

use copperline::whdload::{self, Options, WhdbootAssets};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Local ROM assets: `COPPERLINE_TEST_ASSETS`, else `test-assets/`, else the
/// repo root (tests/README.md).
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let dir = repo_root().join("test-assets");
    if dir.is_dir() {
        return dir;
    }
    repo_root()
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "copperline-whdload-test-{}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Escape a path for inclusion in a double-quoted TOML string.
fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[test]
#[ignore]
fn whdload_boots_the_test_slave_from_an_lha_package() {
    let whdboot_dir = repo_root().join("assets").join("whdboot");
    let whdload_archive = whdboot_dir.join(whdload::WHDLOAD_USR_ARCHIVE);
    if !whdload_archive.is_file() {
        eprintln!(
            "skipping whdload boot; {} not present (run tools/fetch-whdload.sh)",
            whdload_archive.display()
        );
        return;
    }
    let skick = whdboot_dir.join(whdload::SKICK_ARCHIVE);

    let temp = temp_dir("boot");
    let library = temp.join("library");
    let game = repo_root()
        .join("tests")
        .join("assets")
        .join("whdload")
        .join("TestGame.lha");

    // Stage once through the library to decide whether the local collection
    // holds a machine Kickstart; the spawned emulator then restages into the
    // same library (reusing the extraction, as a second launch would).
    let opts = Options {
        library: Some(library.clone()),
        kickstart_dirs: vec![asset_dir()],
        extra_args: None,
        assets: Some(WhdbootAssets {
            whdload_archive: whdload_archive.clone(),
            skick_archive: skick.is_file().then_some(skick),
        }),
    };
    let prepared = whdload::prepare(&game, &opts).expect("staging the fixture package");
    if prepared.machine_rom.is_none() {
        eprintln!(
            "skipping whdload boot; no Kickstart 3.1 (40.068 A1200) image found in {}",
            asset_dir().display()
        );
        return;
    }

    let config = temp.join("whdload.toml");
    std::fs::write(
        &config,
        format!(
            "[whdload]\ngame = \"{}\"\nlibrary = \"{}\"\nkickstarts = \"{}\"\n",
            toml_path(&game),
            toml_path(&library),
            toml_path(&asset_dir()),
        ),
    )
    .unwrap();

    let shot = temp.join("boot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("COPPERLINE_WHDBOOT_DIR", &whdboot_dir)
        .args([
            "--config",
            config.to_str().unwrap(),
            "--noaudio",
            "--screenshot-after",
            "25",
            shot.to_str().unwrap(),
        ])
        .output()
        .expect("running the emulator");
    assert!(
        output.status.success(),
        "emulator failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The slave writes COLOR00 = $0B4 with all DMA off, so the whole frame
    // is that colour ($0B4 expands to 00/BB/44).
    let decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(&shot).expect("screenshot written"),
    ));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().expect("png dimensions")];
    let info = reader.next_frame(&mut buf).unwrap();
    let rgba = &buf[..info.buffer_size()];
    let total = (info.width * info.height) as usize;
    let teal = rgba
        .chunks_exact(4)
        .filter(|px| px[0] == 0x00 && px[1] == 0xBB && px[2] == 0x44)
        .count();
    assert!(
        teal * 10 >= total * 9,
        "expected a solid $0B4 frame, got {teal}/{total} matching pixels"
    );

    std::fs::remove_dir_all(&temp).ok();
}
