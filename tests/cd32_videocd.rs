// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated end-to-end verification for the cartridge-resident
//! `videocd.library`. A real CD32 Kickstart runs the committed Amiga probe,
//! while the library reads the Philips sampler through the public cd.device
//! API. The probe transcript proves the binary ABI and parsed disc metadata.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

fn toml_path(path: &Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

fn png_colours(path: &Path) -> (u32, u32, usize) {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        File::open(path).expect("open Video CD screenshot"),
    ));
    let mut reader = decoder
        .read_info()
        .expect("read Video CD screenshot header");
    let mut bytes = vec![
        0;
        reader
            .output_buffer_size()
            .expect("Video CD screenshot dimensions")
    ];
    let info = reader
        .next_frame(&mut bytes)
        .expect("decode Video CD screenshot");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let distinct = bytes[..info.buffer_size()]
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<HashSet<_>>()
        .len();
    (info.width, info.height, distinct)
}

#[test]
#[ignore = "needs CD32 Kickstart ROMs, the open FMV ROM, and the Philips Video CD"]
fn open_videocd_library_parses_the_philips_sampler() -> Result<(), Box<dyn std::error::Error>> {
    let Some(rom) = env_path("COPPERLINE_FMV_KICKSTART_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_ROM");
        return Ok(());
    };
    let Some(extended) = env_path("COPPERLINE_FMV_KICKSTART_EXT_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_EXT_ROM");
        return Ok(());
    };
    let Some(fmv_rom) = env_path("COPPERLINE_FMV_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_ROM");
        return Ok(());
    };
    let Some(vcd) = env_path("COPPERLINE_FMV_VIDEOCD_CUE") else {
        eprintln!("skipping: set COPPERLINE_FMV_VIDEOCD_CUE");
        return Ok(());
    };
    for asset in [&rom, &extended, &fmv_rom, &vcd] {
        assert!(
            asset.is_file(),
            "required Video CD asset: {}",
            asset.display()
        );
    }

    let scratch =
        std::env::temp_dir().join(format!("copperline-videocd-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let program_dir = scratch.join("program");
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&program_dir)?;
    std::fs::create_dir_all(&config_home)?;
    let probe = program_dir.join("videocdtest");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("guest/videocd-test/videocdtest"),
        &probe,
    )?;

    let config = scratch.join("videocd.toml");
    let screenshot = scratch.join("videocd.png");
    std::fs::write(
        &config,
        format!(
            "rom = {}\nextended_rom = {}\nfmv_rom = {}\n\n\
             [machine]\nprofile = \"CD32\"\n\n\
             [chipset]\nrevision = \"AGA\"\n\n\
             [cd]\nimage = {}\nnvram = {}\n",
            toml_path(&rom),
            toml_path(&extended),
            toml_path(&fmv_rom),
            toml_path(&vcd),
            toml_path(&scratch.join("videocd.nvram")),
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("XDG_CONFIG_HOME", &config_home)
        .env("RUST_LOG", "copperline=warn")
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--run")
        .arg(&probe)
        .arg("--screenshot-after")
        .arg("45")
        .arg(&screenshot)
        .output()?;
    assert!(
        output.status.success(),
        "Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    let report = std::fs::read_to_string(program_dir.join("VIDEOCD-RESULT"))?;
    for expected in [
        "class=00000004",
        "disc_kind=00000101",
        "album=3106906332",
        "tracks=00000002",
        "entries=0000002d",
        "volume_count=00000001",
        "volume_number=00000001",
        "track_kind=00000102",
        "track_number=00000002",
        "start_lsn=00000d7a",
        "end_lsn=00000fd4",
        "VIDEOTEST: PASS",
    ] {
        assert!(
            report.lines().any(|line| line == expected),
            "{expected:?} missing:\n{report}"
        );
    }

    std::fs::remove_dir_all(scratch)?;
    Ok(())
}

#[test]
#[ignore = "needs CD32 Kickstart ROMs, the open FMV ROM, and the Philips Video CD"]
fn video_cd_autoboot_player_plays_and_aborts_track() -> Result<(), Box<dyn std::error::Error>> {
    let Some(rom) = env_path("COPPERLINE_FMV_KICKSTART_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_ROM");
        return Ok(());
    };
    let Some(extended) = env_path("COPPERLINE_FMV_KICKSTART_EXT_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_EXT_ROM");
        return Ok(());
    };
    let Some(fmv_rom) = env_path("COPPERLINE_FMV_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_ROM");
        return Ok(());
    };
    let Some(vcd) = env_path("COPPERLINE_FMV_VIDEOCD_CUE") else {
        eprintln!("skipping: set COPPERLINE_FMV_VIDEOCD_CUE");
        return Ok(());
    };
    for asset in [&rom, &extended, &fmv_rom, &vcd] {
        assert!(
            asset.is_file(),
            "required Video CD asset: {}",
            asset.display()
        );
    }

    let scratch =
        std::env::temp_dir().join(format!("copperline-videocd-player-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&config_home)?;
    let config = scratch.join("videocd-player.toml");
    let playing = scratch.join("playing.png");
    let returned = scratch.join("returned.png");
    std::fs::write(
        &config,
        format!(
            "rom = {}\nextended_rom = {}\nfmv_rom = {}\n\n\
             [machine]\nprofile = \"CD32\"\n\n\
             [chipset]\nrevision = \"AGA\"\n\n\
             [cd]\nimage = {}\nnvram = {}\n",
            toml_path(&rom),
            toml_path(&extended),
            toml_path(&fmv_rom),
            toml_path(&vcd),
            toml_path(&scratch.join("videocd.nvram")),
        ),
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("XDG_CONFIG_HOME", &config_home)
        .env("RUST_LOG", "copperline::cd32_fmv=trace,copperline=warn")
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .args(["--joy-after", "18", "red", "150"])
        .args(["--screenshot-after", "21"])
        .arg(&playing)
        .args(["--joy-after", "22", "blue", "150"])
        .args(["--screenshot-after", "24"])
        .arg(&returned)
        .output()?;
    assert!(
        output.status.success(),
        "Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MPEG sequence 352x240"),
        "player did not find the first Video CD sequence:\n{}",
        tail(&output.stderr, 80)
    );
    let decoded = stderr.matches("decoded MPEG frame 352x240").count();
    assert!(
        decoded >= 20,
        "player decoded only {decoded} Video CD frames:\n{}",
        tail(&output.stderr, 80)
    );
    assert!(
        stderr.matches("I/O control <- 0x3200").count() >= 2,
        "Blue did not hide the decoder output after AbortIO:\n{}",
        tail(&output.stderr, 80)
    );

    let (playing_width, playing_height, playing_colours) = png_colours(&playing);
    let (returned_width, returned_height, returned_colours) = png_colours(&returned);
    assert_eq!((playing_width, playing_height), (716, 540));
    assert_eq!((returned_width, returned_height), (716, 540));
    assert!(
        playing_colours >= 5_000,
        "playing frame is unexpectedly flat ({playing_colours} colours)"
    );
    assert!(
        returned_colours < 1_000,
        "player menu did not return after Blue ({returned_colours} colours)"
    );

    std::fs::remove_dir_all(scratch)?;
    Ok(())
}
