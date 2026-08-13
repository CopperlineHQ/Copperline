//! Asset-gated Picasso96 boot checks for the Picasso II/II+ hardware model.
//!
//! The HDFs are local licensed Workbench/Picasso96 installations and are
//! never committed. See tests/README.md for lookup and setup conventions.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
#[path = "../src/envcfg.rs"]
mod envcfg;

struct Image {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Image {
    fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let decoder = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
        let mut reader = decoder.read_info()?;
        let size = reader
            .output_buffer_size()
            .ok_or("PNG dimensions overflow")?;
        let mut bytes = vec![0; size];
        let info = reader.next_frame(&mut bytes)?;
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        Ok(Self {
            width: info.width,
            height: info.height,
            rgba: bytes[..info.buffer_size()].to_vec(),
        })
    }

    fn distinct_colors(&self) -> usize {
        self.rgba
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect::<HashSet<_>>()
            .len()
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn asset_dir() -> PathBuf {
    if let Some(dir) = envcfg::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let local = repo_root().join("test-assets");
    if local.is_dir() {
        local
    } else {
        repo_root()
    }
}

/// The generic A500/A2000 Kickstart 3.1 has no A3000/A4000 SCSI driver, so
/// these boots use an A4000 (a big-box machine with Zorro slots) with its own
/// ROM and the motherboard IDE port the HDF images were installed against.
const KICK31_A4000: &str = "Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom";

fn run_picasso_hdf(
    name: &str,
    hdf: &str,
    card: &str,
) -> Result<Option<(Image, String)>, Box<dyn std::error::Error>> {
    let assets = asset_dir();
    let missing: Vec<_> = [KICK31_A4000, hdf]
        .into_iter()
        .filter(|file| !assets.join(file).is_file())
        .collect();
    if !missing.is_empty() {
        eprintln!("skipping Picasso II/II+ integration test; missing files: {missing:?}");
        return Ok(None);
    }

    let stem = format!("copperline-picasso2-{name}-{}", std::process::id());
    let config = std::env::temp_dir().join(format!("{stem}.toml"));
    let screenshot = std::env::temp_dir().join(format!("{stem}.png"));
    std::fs::write(
        &config,
        format!(
            r#"rom = "{KICK31_A4000}"

[machine]
profile = "A4000"

[rtg]
card = "{card}"
vram = "2M"

[ide]
master = "{hdf}"
"#,
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&assets)
        .env(
            "RUST_LOG",
            "copperline=warn,copperline::picasso2=info,copperline::emulator=info",
        )
        .env("COPPERLINE_DIAG_PICASSO", "1")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("120")
        .arg(&screenshot)
        .output()?;
    let _ = std::fs::remove_file(config);
    assert!(
        output.status.success(),
        "Copperline failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let image = Image::load(&screenshot)?;
    let _ = std::fs::remove_file(screenshot);
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok(Some((image, log)))
}

fn assert_workbench_frame(image: &Image) {
    assert_eq!(image.width, 716, "headless RTG path scales to FB_WIDTH");
    assert_eq!(image.height, 480, "expected a 640x480 Picasso screen");
    assert!(
        image.distinct_colors() >= 8,
        "expected a nonblank Picasso96 Workbench frame"
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart/Picasso96 HDF"]
fn picasso2_workbench_opens_640x480x8() -> Result<(), Box<dyn std::error::Error>> {
    let Some((image, log)) = run_picasso_hdf("clut8", "p96-picasso2.hdf", "picasso2")? else {
        return Ok(());
    };
    assert_workbench_frame(&image);
    assert!(
        log.contains("Clut8"),
        "diagnostic trace never decoded 8-bit mode"
    );
    Ok(())
}

#[test]
#[ignore = "runs the emulator and requires a local truecolour Picasso96 HDF"]
fn picasso2_workbench_opens_640x480x16() -> Result<(), Box<dyn std::error::Error>> {
    let Some((image, log)) = run_picasso_hdf("rgb565", "p96-picasso2-16.hdf", "picasso2")? else {
        return Ok(());
    };
    assert_workbench_frame(&image);
    assert!(
        log.contains("Rgb565"),
        "diagnostic trace never decoded 16-bit mode"
    );
    Ok(())
}

#[test]
#[ignore = "runs the emulator and requires a local Picasso96 HDF"]
fn picasso2plus_workbench_opens_with_gd5428_revision() -> Result<(), Box<dyn std::error::Error>> {
    let Some((image, log)) = run_picasso_hdf("plus-clut8", "p96-picasso2.hdf", "picasso2plus")?
    else {
        return Ok(());
    };
    assert_workbench_frame(&image);
    assert!(
        log.contains("Clut8"),
        "diagnostic trace never decoded the II+ screen"
    );
    assert!(
        log.contains("CL-GD5428 RTG board"),
        "emulator did not instantiate the II+ controller revision"
    );
    Ok(())
}

#[test]
#[ignore = "runs guest-side p96cts and requires its prepared local HDF"]
fn picasso2_p96cts_reports_all_modes_clean() -> Result<(), Box<dyn std::error::Error>> {
    let assets = asset_dir();
    let hdf = "p96-picasso2-cts.hdf";
    let missing: Vec<_> = [KICK31_A4000, hdf]
        .into_iter()
        .filter(|file| !assets.join(file).is_file())
        .collect();
    if !missing.is_empty() {
        eprintln!("skipping Picasso II p96cts; missing files: {missing:?}");
        return Ok(());
    }

    let stem = format!("copperline-picasso2-p96cts-{}", std::process::id());
    let output_dir = std::env::temp_dir().join(&stem);
    let config = std::env::temp_dir().join(format!("{stem}.toml"));
    let screenshot = std::env::temp_dir().join(format!("{stem}.png"));
    std::fs::create_dir_all(&output_dir)?;
    let output_toml = toml::Value::String(output_dir.to_string_lossy().into_owned()).to_string();
    std::fs::write(
        &config,
        format!(
            r#"rom = "{KICK31_A4000}"

[machine]
profile = "A4000"

[rtg]
card = "picasso2"
vram = "2M"

[ide]
master = "{hdf}"

[[filesys]]
path = {output_toml}
volume = "P96OUT"
"#,
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&assets)
        .env("RUST_LOG", "copperline=warn")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("600")
        .arg(&screenshot)
        .output()?;
    let _ = std::fs::remove_file(config);
    let _ = std::fs::remove_file(screenshot);
    assert!(
        output.status.success(),
        "Copperline failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let marker = output_dir.join("p96cts.result");
    let result = std::fs::read_to_string(&marker).unwrap_or_else(|error| {
        panic!(
            "p96cts did not write {}: {error}; output directory contains {:?}",
            marker.display(),
            std::fs::read_dir(&output_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.file_name())
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        result.trim(),
        "PASS",
        "p96cts reported failures in {output_dir:?}"
    );
    let _ = std::fs::remove_dir_all(output_dir);
    Ok(())
}
