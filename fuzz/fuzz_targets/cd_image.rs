//! Fuzz the CD image loaders (CUE sheet, bare ISO, NRG, CHD) by writing the
//! input to a temporary file named by its extension and loading it. The
//! CUE parser follows BINARY file references, so the bytes are also laid
//! down under the name a cue sheet would point at.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;

fn load_as(data: &[u8], extension: &str) {
    let Ok(dir) = tempfile::Builder::new()
        .prefix("copperline-fuzz-cd-")
        .tempdir()
    else {
        return;
    };
    let path: PathBuf = dir.path().join(format!("image.{extension}"));
    if std::fs::write(&path, data).is_err() {
        return;
    }
    if extension == "cue" {
        // A CUE sheet's FILE entries resolve relative to the sheet, so give
        // the parser a sibling to find.
        let _ = std::fs::write(dir.path().join("image.bin"), data);
    }
    // Errors are fine; panics, hangs, and over-allocation are not.
    let _ = copperline::cdrom::CdImage::load(&path);
}

fuzz_target!(|data: &[u8]| {
    load_as(data, "cue");
    load_as(data, "iso");
    load_as(data, "nrg");
    load_as(data, "chd");
});
