// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated end-to-end boot of Copperline's bundled clean-room A2091 ROM.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn asset_dir() -> PathBuf {
    std::env::var_os("COPPERLINE_A2091_TEST_ASSETS")
        .or_else(|| std::env::var_os("COPPERLINE_TEST_ASSETS"))
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("test-assets"))
}

fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn distinct_colors(path: &Path) -> Result<usize, Box<dyn std::error::Error>> {
    let decoder = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG dimensions overflow")?;
    let mut data = vec![0; size];
    let info = reader.next_frame(&mut data)?;
    let bytes = &data[..info.buffer_size()];
    let stride = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err("unexpected screenshot pixel format".into()),
    };
    Ok(bytes
        .chunks_exact(stride)
        .map(|pixel| pixel[..3].to_vec())
        .collect::<HashSet<_>>()
        .len())
}

fn make_directory_volume(parent: &Path, name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let volume = parent.join(name);
    std::fs::create_dir_all(&volume)?;
    std::fs::copy(
        repo_root().join("guest/hostfs-test/mkfile"),
        volume.join("mkfile"),
    )?;
    Ok(volume)
}

#[test]
#[ignore = "runs the emulator and requires local Kickstart/RDB assets"]
fn a2091_open_rom_boots_amigasys_under_kick31() -> Result<(), Box<dyn std::error::Error>> {
    let assets = asset_dir();
    let kick = assets.join("KICK31.ROM");
    let source_hdf = assets.join("AmigaSYS3PlusAGA-rdb.hdf");
    if !kick.is_file() || !source_hdf.is_file() {
        eprintln!(
            "skipping A2091 boot; missing {} or {}",
            kick.display(),
            source_hdf.display()
        );
        return Ok(());
    }

    let temp = std::env::temp_dir().join(format!(
        "copperline-a2091-boot-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp)?;
    let hdf = temp.join("work.hdf");
    std::fs::copy(&source_hdf, &hdf)?;
    let cfg = temp.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"[machine]
profile = "A1200"

[cpu]
model = "68030"

[memory]
fast = "8M"

[scsi]
controller = "a2091"
unit0 = "{}"
"#,
            toml_path(&hdf)
        ),
    )?;

    let png = temp.join("boot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("COPPERLINE_A2091_DIR", repo_root().join("assets/a2091"))
        .env("COPPERLINE_DIAG_A2091", "1")
        .env("RUST_LOG", "copperline=info")
        .arg("--factory")
        .arg("--config")
        .arg(&cfg)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("120")
        .arg(&png)
        .arg(&kick)
        .output()?;
    assert!(
        output.status.success(),
        "copperline failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        log.contains("no A2091 ROM specified; using bundled open ROM"),
        "bundled A2091 ROM was not resolved:\n{log}"
    );
    assert!(
        log.contains("a2091 rd 0x00E0/2"),
        "driver never started the DMAC:\n{log}"
    );
    assert!(
        log.contains("a2091 wr 0x0084/2") && log.contains("a2091 wr 0x0086/2"),
        "driver never programmed a 24-bit DMA address:\n{log}"
    );
    assert!(
        log.contains("a2091 rd 0x0041/1 -> 0x00D1"),
        "driver never observed a delivered WD33C93 INT2 status:\n{log}"
    );
    assert!(
        distinct_colors(&png)? > 128,
        "Workbench screenshot is blank"
    );

    std::fs::remove_dir_all(&temp).ok();
    Ok(())
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 1.3 ROM"]
fn a2091_open_rom_boots_under_kick13() -> Result<(), Box<dyn std::error::Error>> {
    let kick = asset_dir().join("KICK13.ROM");
    if !kick.is_file() {
        eprintln!(
            "skipping A2091 Kickstart 1.3 boot; missing {}",
            kick.display()
        );
        return Ok(());
    }

    let temp = std::env::temp_dir().join(format!(
        "copperline-a2091-kick13-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp)?;
    let volume = make_directory_volume(&temp, "kick13-volume")?;
    let hdf = temp.join("kick13.hdf");
    let image = copperline::dirfs::build_image(
        &volume,
        "A2091Test",
        copperline::diskimage::FileSystem::OFS,
    )?;
    std::fs::write(&hdf, image)?;
    let cfg = temp.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"[machine]
profile = "A500"

[cpu]
model = "68000"

[memory]
chip = "512K"
fast = "8M"

[chipset]
revision = "OCS"

[scsi]
controller = "a2091"
unit0 = "{}"
"#,
            toml_path(&hdf)
        ),
    )?;

    let png = temp.join("boot.png");
    let mut command = Command::new(env!("CARGO_BIN_EXE_copperline"));
    command
        .env("COPPERLINE_A2091_DIR", repo_root().join("assets/a2091"))
        .env("RUST_LOG", "copperline=info")
        .arg("--factory")
        .arg("--config")
        .arg(&cfg)
        .arg("--noaudio");
    for (i, key) in ["m", "k", "f", "i", "l", "e", "return"].iter().enumerate() {
        command
            .arg("--press-after")
            .arg(format!("{:.1}", 40.0 + 0.3 * i as f64))
            .arg(key);
    }
    let output = command
        .arg("--screenshot-after")
        .arg("50")
        .arg(&png)
        .arg(&kick)
        .output()?;
    assert!(
        output.status.success(),
        "copperline failed: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(
        log.contains("no A2091 ROM specified; using bundled open ROM"),
        "bundled A2091 ROM was not resolved:\n{log}"
    );
    assert!(
        distinct_colors(&png)? >= 3,
        "Kickstart 1.3 boot screenshot is blank"
    );
    let written = std::fs::read(&hdf)?;
    assert!(
        written
            .windows(b"hello from the guest\n".len())
            .any(|window| window == b"hello from the guest\n"),
        "guest write did not reach the file-backed OFS hardfile"
    );

    std::fs::remove_dir_all(&temp).ok();
    Ok(())
}

#[test]
#[ignore = "runs the emulator"]
fn a2091_open_rom_boots_under_aros() -> Result<(), Box<dyn std::error::Error>> {
    let temp = std::env::temp_dir().join(format!(
        "copperline-a2091-aros-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp)?;
    let volume = make_directory_volume(&temp, "aros-volume")?;
    let cfg = temp.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"[machine]
profile = "A500"

[cpu]
model = "68000"

[memory]
chip = "2M"
fast = "8M"

[chipset]
revision = "ECS"

[scsi]
controller = "a2091"
unit0 = "{}"
"#,
            toml_path(&volume)
        ),
    )?;

    let png = temp.join("boot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .env("COPPERLINE_A2091_DIR", repo_root().join("assets/a2091"))
        .arg("--factory")
        .arg("--config")
        .arg(&cfg)
        .arg("--noaudio")
        .arg("--serial")
        .arg("stdout")
        .arg("--screenshot-after")
        .arg("35")
        .arg(&png)
        .output()?;
    assert!(
        output.status.success(),
        "copperline failed: {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let serial = String::from_utf8_lossy(&output.stdout);
    assert!(
        serial.contains("mfg=514 prod=3")
            && serial.contains("InitResident")
            && serial.contains("Copperline A2091 scsidisk 42.40"),
        "AROS did not initialise the bundled A2091 resident:\n{serial}"
    );
    assert!(distinct_colors(&png)? >= 3, "AROS boot screenshot is blank");

    std::fs::remove_dir_all(&temp).ok();
    Ok(())
}
