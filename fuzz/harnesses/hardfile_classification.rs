//! Fuzz hardfile opening and classification: `HardDriveImage::open` validates
//! raw-image sizing, sniffs the first 16 sectors for `RDSK`, and distinguishes
//! those images from bare DOS volumes that need a synthesized RDB overlay.
//! The same open sniffs gzip and CHD magic and the VHD footer, so a hostile
//! `.hdz`, a CHD header claiming a petabyte of hunks, or a dynamic VHD whose
//! block table points anywhere reaches their parsers here too; the cheap CHD
//! media classifier the configuration uses gets the bytes as well.

use std::path::PathBuf;

pub fn exercise(data: &[u8]) {
    let dir = tempfile::Builder::new()
        .prefix("copperline-fuzz-hdf-")
        .tempdir()
        .expect("create hardfile fuzz directory");
    let path: PathBuf = dir.path().join("image.hdf");
    std::fs::write(&path, data).expect("write hardfile fuzz input");
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
    // A CHD hard disk opened writable creates its overlay sidecar in the
    // temporary directory; it goes with the directory.
    let _kind = copperline::harddrive::chd::media_kind(&path);
}
