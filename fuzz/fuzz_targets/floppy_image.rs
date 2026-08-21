//! Fuzz the floppy image decoder through its single public entry point,
//! which dispatches on magic bytes across ADF, UAE extended ADF, DMS, SCP
//! flux, IPF, and the gzip/zip containers.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;

fuzz_target!(|data: &[u8]| {
    let mut controller = copperline::floppy::FloppyController::default();
    // Errors are fine; panics, hangs, and over-allocation are not. The
    // label is only carried for diagnostics, so a fixed name is fine.
    let _ = controller.insert_disk_image_bytes(
        0,
        data.to_vec(),
        PathBuf::from("fuzz"),
        false,
    );
});
