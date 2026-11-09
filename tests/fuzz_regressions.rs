// SPDX-License-Identifier: GPL-3.0-or-later

//! Replay retained fuzz inputs through the same harnesses as libFuzzer.
//! No external assets, nightly compiler or sanitizer runtime is required.

use std::path::Path;

#[path = "../fuzz/harnesses/archives.rs"]
mod archives;
#[path = "../fuzz/harnesses/cd_image.rs"]
mod cd_image;
#[path = "../fuzz/harnesses/config_toml.rs"]
mod config_toml;
#[cfg(feature = "control")]
#[path = "../fuzz/harnesses/control_protocol.rs"]
mod control_protocol;
#[cfg(feature = "dap")]
#[path = "../fuzz/harnesses/debug_info.rs"]
mod debug_info;
#[path = "../fuzz/harnesses/dms.rs"]
mod dms;
#[path = "../fuzz/harnesses/floppy_image.rs"]
mod floppy_image;
#[path = "../fuzz/harnesses/hardfile_classification.rs"]
mod hardfile_classification;
#[path = "../fuzz/harnesses/savestate.rs"]
mod savestate;

fn replay(target: &str, exercise: fn(&[u8])) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fuzz-regressions")
        .join(target);
    let mut cases: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "{target} has no retained replay inputs");
    for case in cases {
        assert!(case.is_file(), "unexpected replay path {}", case.display());
        eprintln!("replaying {}", case.display());
        exercise(&std::fs::read(&case).unwrap());
    }
}

macro_rules! replay_test {
    ($target:ident) => {
        #[test]
        fn $target() {
            replay(stringify!($target), $target::exercise);
        }
    };
}

replay_test!(archives);
replay_test!(config_toml);
#[cfg(feature = "control")]
replay_test!(control_protocol);
#[cfg(feature = "dap")]
replay_test!(debug_info);
replay_test!(dms);
replay_test!(floppy_image);
replay_test!(cd_image);
replay_test!(hardfile_classification);
replay_test!(savestate);
