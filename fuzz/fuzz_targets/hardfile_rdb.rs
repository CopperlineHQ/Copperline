//! Fuzz the hardfile/RDB layer: HardDriveImage::open classifies a bare
//! filesystem volume versus an RDB-partitioned image and parses the RDSK
//! partition table, all from attacker-supplied bytes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static ITERATION: AtomicU64 = AtomicU64::new(0);

fuzz_target!(|data: &[u8]| {
    let n = ITERATION.fetch_add(1, Ordering::Relaxed);
    // The process id keeps parallel -jobs/-workers processes out of each
    // other's temporary directories.
    let dir = std::env::temp_dir().join(format!("copperline-fuzz-hdf-{}-{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path: PathBuf = dir.join("image.hdf");
    if std::fs::write(&path, data).is_err() {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // Errors are fine; panics, hangs, and over-allocation are not. The
    // smallest useful image is one RDB cylinder; anything smaller is
    // rejected, which is exactly the path under test.
    let result = copperline::harddrive::HardDriveImage::open(
        &path,
        "fuzz",
        "fuzz",
        None,
        0,
        copperline::diskimage::FileSystem::FFS,
    );
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(image) => drop(image),
        Err(_) => {}
    }
});
