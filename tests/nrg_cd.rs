// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated checks of the NRG CD backend against a real Nero 5 DAO image
//! with one MODE1/2048 data track and CD audio tracks. The synthetic unit
//! tests cover both footer generations and DAO/TAO layout mechanics; this
//! regression proves the real CD32 image's footer offsets, ISO data, and
//! CD-DA byte order.

use copperline::cdrom::{CdImage, DATA_SECTOR_BYTES, RAW_SECTOR_BYTES};

fn asset(name: &str) -> Option<std::path::PathBuf> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = match std::env::var_os("COPPERLINE_TEST_ASSETS") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            let dir = root.join("test-assets");
            if dir.is_dir() {
                dir
            } else {
                root
            }
        }
    };
    let path = dir.join(name);
    path.is_file().then_some(path)
}

fn lag1_autocorrelation(sectors: &[[u8; RAW_SECTOR_BYTES]]) -> f64 {
    let samples: Vec<f64> = sectors
        .iter()
        .flat_map(|sector| sector.chunks_exact(4))
        .map(|frame| f64::from(i16::from_le_bytes([frame[0], frame[1]])))
        .collect();
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    let numerator: f64 = samples
        .windows(2)
        .map(|window| (window[0] - mean) * (window[1] - mean))
        .sum();
    let denominator: f64 = samples
        .iter()
        .map(|sample| (sample - mean) * (sample - mean))
        .sum();
    numerator / denominator
}

#[test]
#[ignore = "needs the local CD32 NRG image (see tests/README.md)"]
fn nrg_cd32_disc_serves_iso9660_data_and_smooth_audio() {
    let Some(path) = asset("30 Games Compilation CD (2005)(Stuermer, A.).nrg") else {
        eprintln!("skipping: CD32 NRG not in the asset directory");
        return;
    };
    let mut image = CdImage::load(&path).expect("NRG loads");
    assert_eq!(image.tracks().len(), 10);
    assert!(image.tracks()[0].kind.is_data());
    let audio_start = image
        .tracks()
        .iter()
        .find(|track| !track.kind.is_data())
        .expect("disc has an audio track")
        .start_sector;

    let mut pvd = [0u8; DATA_SECTOR_BYTES];
    image.read_data_sector(16, &mut pvd).expect("PVD reads");
    assert_eq!(&pvd[1..6], b"CD001", "ISO9660 identifier at LBA 16");

    let mut sectors = vec![[0u8; RAW_SECTOR_BYTES]; 75];
    let mut peak = 0i16;
    for (index, sector) in sectors.iter_mut().enumerate() {
        image
            .read_audio_sector(audio_start + 300 + index as u32, sector)
            .expect("audio sector reads");
        for sample in sector.chunks_exact(2) {
            peak = peak.max(i16::from_le_bytes([sample[0], sample[1]]).saturating_abs());
        }
    }
    assert!(peak > 1000, "audio window is not silence (peak {peak})");
    let correlation = lag1_autocorrelation(&sectors);
    assert!(
        correlation > 0.5,
        "CD-DA is not smooth little-endian PCM (lag-1 autocorrelation {correlation:.3})"
    );
}
