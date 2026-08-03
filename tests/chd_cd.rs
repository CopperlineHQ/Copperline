// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated checks of the CHD CD backend against a real chdman image
//! (a CD32 game disc with one MODE1_RAW data track and CD audio tracks;
//! Pinball Fantasies is the local regression example). The unit tests in
//! `src/cdrom/chd.rs` cover layout and byte order with synthesized
//! uncompressed CHDs; this test proves the compressed-codec path: hunk
//! decompression, frame addressing, and CD-DA byte order on real data.

use copperline::cdrom::{CdImage, DATA_SECTOR_BYTES, RAW_SECTOR_BYTES};

/// The integration-test asset directory (see `tests/README.md`):
/// `COPPERLINE_TEST_ASSETS`, else `test-assets/` under the repo root,
/// else the repo root itself.
fn asset(name: &str) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = match std::env::var_os("COPPERLINE_TEST_ASSETS") {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let d = root.join("test-assets");
            if d.is_dir() {
                d
            } else {
                root
            }
        }
    };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

/// Lag-1 autocorrelation of the left channel of little-endian CD-DA
/// samples: near 1 for band-limited audio, near 0 for the byte-shuffled
/// noise a byte-order mistake produces.
fn lag1_autocorrelation(sectors: &[[u8; RAW_SECTOR_BYTES]]) -> f64 {
    let samples: Vec<f64> = sectors
        .iter()
        .flat_map(|s| s.chunks_exact(4))
        .map(|frame| f64::from(i16::from_le_bytes([frame[0], frame[1]])))
        .collect();
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let num: f64 = samples
        .windows(2)
        .map(|w| (w[0] - mean) * (w[1] - mean))
        .sum();
    let den: f64 = samples.iter().map(|s| (s - mean) * (s - mean)).sum();
    num / den
}

#[test]
#[ignore = "needs a local CD32 game CHD (see tests/README.md)"]
fn chd_cd32_disc_serves_iso9660_data_and_smooth_audio() {
    let Some(path) = asset("Pinball Fantasies (EU).chd") else {
        eprintln!("skipping: CD32 game CHD not in the asset directory");
        return;
    };
    let mut image = CdImage::load(&path).expect("CHD loads");

    let tracks = image.tracks().to_vec();
    assert!(tracks[0].kind.is_data(), "track 1 is the data track");
    let audio_track = tracks
        .iter()
        .find(|t| !t.kind.is_data())
        .expect("disc has a CD audio track");

    // The ISO9660 primary volume descriptor sits at LBA 16 of the data
    // track: decompressed user data must carry the standard identifier.
    let mut pvd = [0u8; DATA_SECTOR_BYTES];
    image.read_data_sector(16, &mut pvd).expect("PVD reads");
    assert_eq!(&pvd[1..6], b"CD001", "ISO9660 identifier at LBA 16");

    // Pull a second of audio from inside the first audio track (well past
    // the pregap, where the music has started) and check it decodes to
    // smooth PCM in disc byte order rather than byte-swapped noise.
    let mut sectors = vec![[0u8; RAW_SECTOR_BYTES]; 75];
    let start = audio_track.start_sector + 300;
    let mut peak = 0i16;
    for (i, sector) in sectors.iter_mut().enumerate() {
        image
            .read_audio_sector(start + i as u32, sector)
            .expect("audio sector reads");
        for frame in sector.chunks_exact(2) {
            peak = peak.max(i16::from_le_bytes([frame[0], frame[1]]).saturating_abs());
        }
    }
    assert!(peak > 1000, "audio window is not silence (peak {peak})");
    let corr = lag1_autocorrelation(&sectors);
    assert!(
        corr > 0.5,
        "CD-DA does not decode to smooth audio (lag-1 autocorrelation {corr:.3})"
    );
}
