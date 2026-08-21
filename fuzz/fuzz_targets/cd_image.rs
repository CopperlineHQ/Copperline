//! Fuzz the CD image loaders (CUE sheet, bare ISO, CHD) by writing the
//! input to a temporary file named by its extension and loading it. The
//! CUE parser follows BINARY file references, so the bytes are also laid
//! down under the name a cue sheet would point at.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static ITERATION: AtomicU64 = AtomicU64::new(0);

fn load_as(data: &[u8], extension: &str) {
    let n = ITERATION.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("copperline-fuzz-cd-{n}"));
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path: PathBuf = dir.join(format!("image.{extension}"));
    if std::fs::write(&path, data).is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    if extension == "cue" {
        // A CUE sheet's FILE entries resolve relative to the sheet, so give
        // the parser a sibling to find.
        let _ = std::fs::write(dir.join("image.bin"), data);
    }
    // Errors are fine; panics, hangs, and over-allocation are not.
    let _ = copperline::cdrom::CdImage::load(&path);
    let _ = std::fs::remove_dir_all(&dir);
}

fuzz_target!(|data: &[u8]| {
    load_as(data, "cue");
    load_as(data, "iso");
});
