// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated end-to-end regressions for the open CD32 FMV ROM.
//!
//! The AROS case exercises PR 1089 through chronological CDXL commit
//! `ebfc7d9`. The
//! Kickstart case exercises the ROM's own resident `cd32mpeg.device` and its
//! standard Mode-2 `CD_READ` streamer. Both run the real Cannon Fodder
//! introduction far enough to prove sustained video and audio playback. See
//! `tests/README.md` for the required environment variables.

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

fn tail(text: &str, line_count: usize) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

fn wav_peak(path: &Path) -> f32 {
    let mut reader = hound::WavReader::open(path).expect("open WAV capture");
    let spec = reader.spec();
    assert_eq!(spec.channels, 2);
    assert_eq!(spec.sample_format, hound::SampleFormat::Float);
    assert_eq!(spec.bits_per_sample, 32);
    reader
        .samples::<f32>()
        .map(|sample| sample.expect("read WAV sample").abs())
        .fold(0.0, f32::max)
}

fn png_colours(path: &Path) -> (u32, u32, usize) {
    let decoder = png::Decoder::new(std::io::BufReader::new(
        File::open(path).expect("open FMV screenshot"),
    ));
    let mut reader = decoder.read_info().expect("read FMV screenshot header");
    let mut bytes = vec![
        0;
        reader
            .output_buffer_size()
            .expect("FMV screenshot dimensions")
    ];
    let info = reader
        .next_frame(&mut bytes)
        .expect("decode FMV screenshot");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    let distinct = bytes[..info.buffer_size()]
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<HashSet<_>>()
        .len();
    (info.width, info.height, distinct)
}

fn assert_cannon_fmv(
    label: &str,
    fmv_rom: &Path,
    cue: &Path,
    boot_roms: Option<(&Path, &Path)>,
    aros_dir: Option<&Path>,
    min_frames: usize,
) {
    let scratch = std::env::temp_dir().join(format!(
        "copperline-cd32-fmv-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("config")).expect("create FMV test scratch directory");
    let config = scratch.join("cannon-fodder.toml");
    let screenshot = scratch.join("cannon-fodder.png");
    let wav = scratch.join("cannon-fodder.wav");
    let nvram = scratch.join("cannon-fodder.nvram");
    let rom_config = boot_roms.map_or_else(String::new, |(rom, extended)| {
        format!(
            "rom = {}\nextended_rom = {}\n",
            toml_path(rom),
            toml_path(extended)
        )
    });
    std::fs::write(
        &config,
        format!(
            "{rom_config}fmv_rom = {}\n\n[machine]\nprofile = \"CD32\"\n\n[chipset]\nrevision = \"AGA\"\n\n[cd]\nimage = {}\nnvram = {}\n",
            toml_path(fmv_rom),
            toml_path(cue),
            toml_path(&nvram),
        ),
    )
    .expect("write FMV test config");

    let mut command = Command::new(env!("CARGO_BIN_EXE_copperline"));
    command
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("RUST_LOG", "copperline::cd32_fmv=trace,copperline=warn");
    if let Some(aros_dir) = aros_dir {
        command.env("COPPERLINE_AROS_DIR", aros_dir);
    }
    let output = command
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--audio-wav")
        .arg(&wav)
        .arg("--screenshot-after")
        .arg("60")
        .arg(&screenshot)
        .output()
        .expect("run Copperline FMV regression");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr_tail = tail(&stderr, 80);
    assert!(
        output.status.success(),
        "Copperline exited with {}\n{}",
        output.status,
        stderr_tail
    );
    assert!(
        !stderr.contains("resetting malformed MPEG-1 stream"),
        "CL450 input lost MPEG bytes:\n{stderr_tail}"
    );
    let decoded = stderr.matches("decoded MPEG frame 352x288").count();
    assert!(
        decoded >= min_frames,
        "expected sustained Cannon Fodder FMV, decoded {decoded} frames\n{stderr_tail}"
    );

    let (width, height, distinct) = png_colours(&screenshot);
    assert_eq!(height, 540);
    assert!(
        (704..=720).contains(&width),
        "unexpected PAL capture width {width}"
    );
    assert!(
        distinct >= 1_000,
        "FMV screenshot is unexpectedly flat ({distinct} colours)"
    );
    assert!(
        wav_peak(&wav) > 0.05,
        "FMV run did not produce non-silent stereo output"
    );

    std::fs::remove_dir_all(scratch).expect("remove FMV test scratch directory");
}

#[test]
#[ignore = "needs PR 1089 AROS ROMs, the open FMV ROM, and Cannon Fodder CD assets"]
fn cannon_fodder_streams_cleanly_through_the_aros_open_rom() {
    let Some(aros_dir) = env_path("COPPERLINE_FMV_AROS_DIR") else {
        eprintln!("skipping: set COPPERLINE_FMV_AROS_DIR to the PR 1089 ROM directory");
        return;
    };
    let Some(fmv_rom) = env_path("COPPERLINE_FMV_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_ROM to copperline-fmv.rom");
        return;
    };
    let Some(cue) = env_path("COPPERLINE_FMV_CANNON_FODDER_CUE") else {
        eprintln!("skipping: set COPPERLINE_FMV_CANNON_FODDER_CUE to the game CUE");
        return;
    };
    for asset in [
        aros_dir.join("aros-amiga-m68k-rom.bin"),
        aros_dir.join("aros-amiga-m68k-ext.bin"),
        fmv_rom.clone(),
        cue.clone(),
    ] {
        assert!(
            asset.is_file(),
            "required FMV test asset: {}",
            asset.display()
        );
    }

    assert_cannon_fmv("aros", &fmv_rom, &cue, None, Some(&aros_dir), 1_000);
}

#[test]
#[ignore = "needs CD32 Kickstart ROMs, the open FMV ROM, and Cannon Fodder CD assets"]
fn cannon_fodder_streams_cleanly_through_the_standalone_kickstart_rom() {
    let Some(rom) = env_path("COPPERLINE_FMV_KICKSTART_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_ROM to the CD32 Kickstart ROM");
        return;
    };
    let Some(extended) = env_path("COPPERLINE_FMV_KICKSTART_EXT_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_KICKSTART_EXT_ROM to the CD32 extended ROM");
        return;
    };
    let Some(fmv_rom) = env_path("COPPERLINE_FMV_ROM") else {
        eprintln!("skipping: set COPPERLINE_FMV_ROM to copperline-fmv.rom");
        return;
    };
    let Some(cue) = env_path("COPPERLINE_FMV_CANNON_FODDER_CUE") else {
        eprintln!("skipping: set COPPERLINE_FMV_CANNON_FODDER_CUE to the game CUE");
        return;
    };
    for asset in [&rom, &extended, &fmv_rom, &cue] {
        assert!(
            asset.is_file(),
            "required FMV test asset: {}",
            asset.display()
        );
    }

    assert_cannon_fmv(
        "kickstart",
        &fmv_rom,
        &cue,
        Some((&rom, &extended)),
        None,
        700,
    );
}
