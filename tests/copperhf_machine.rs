// SPDX-License-Identifier: GPL-3.0-or-later

//! `[copperhf]` wiring: a configured unit builds into a machine with a
//! `BoardDevice::Copperhf` on the Zorro chain, over the bundled AROS ROM so
//! this needs no local assets and runs in default CI (see
//! `tests/savestate_roundtrip.rs` for the same bundled-ROM pattern).

use copperline::audio::NullSink;
use copperline::config::{Config, DriveImage};
use copperline::emulator::build_machine;
use copperline::zorro_device::BoardDevice;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pin_bundled_aros() {
    std::env::set_var("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"));
}

/// A 256 KiB bare hardfile: the smallest image the shared harddrive layer
/// accepts (one RDB-less virtual cylinder; anything not a multiple of
/// 256 KiB is a hard error).
fn temp_hardfile(name: &str) -> PathBuf {
    static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "copperline-copperhf-machine-{}-{unique}-{name}",
        std::process::id()
    ));
    std::fs::write(&path, vec![0u8; 256 * 1024]).unwrap();
    path
}

fn make_config(unit0_path: &Path) -> anyhow::Result<Config> {
    let mut cfg = Config::default();
    copperline::config::resolve_bundled_rom(&mut cfg)?;
    cfg.copperhf.units[0] = Some(DriveImage {
        path: unit0_path.to_path_buf(),
        ..Default::default()
    });
    Ok(cfg)
}

#[test]
fn configured_unit_wires_a_copperhf_board_into_the_machine() {
    pin_bundled_aros();
    let image = temp_hardfile("unit0.hdf");
    let cfg = make_config(&image).expect("config builds");
    assert!(cfg.copperhf.enabled());

    let emu = build_machine(&cfg, Box::new(NullSink), false, false).expect("machine builds");
    let found = emu
        .bus()
        .devices
        .iter()
        .any(|d| matches!(d, BoardDevice::Copperhf(_)));
    assert!(
        found,
        "expected a BoardDevice::Copperhf on the Zorro chain when [copperhf] configures a unit"
    );

    let _ = std::fs::remove_file(&image);
}
