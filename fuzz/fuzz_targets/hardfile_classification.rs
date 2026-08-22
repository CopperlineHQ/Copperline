//! Fuzz hardfile opening and classification: `HardDriveImage::open` validates
//! raw-image sizing, sniffs the first 16 sectors for `RDSK`, and distinguishes
//! those images from bare DOS volumes that need a synthesized RDB overlay.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;

fuzz_target!(|data: &[u8]| {
    let Ok(dir) = tempfile::Builder::new()
        .prefix("copperline-fuzz-hdf-")
        .tempdir()
    else {
        return;
    };
    let path: PathBuf = dir.path().join("image.hdf");
    if std::fs::write(&path, data).is_err() {
        return;
    }
    // Errors are fine; panics, hangs, and over-allocation are not. This path
    // classifies existing RDSK images but does not decode their partition
    // tables; bare DOS volumes additionally exercise synthesized-RDB setup.
    let _result = copperline::harddrive::HardDriveImage::open(
        &path,
        "fuzz",
        "fuzz",
        None,
        0,
        copperline::diskimage::FileSystem::FFS,
    );
});
