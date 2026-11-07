// SPDX-License-Identifier: GPL-3.0-or-later

//! `flyerclip`: video files to NewTek Video Flyer clips and back.
//!
//! ```text
//! flyerclip import movie.mp4 [clip] [--hq5|--hq6] [--no-audio] [--seconds N]
//! flyerclip export clip [movie.mp4]
//! flyerclip info clip...
//! ```
//!
//! Drag a video file onto flyerclip.exe to import it, or a clip to export
//! it. Without an output name, an imported clip goes onto the Flyer's drive
//! FA0, the `Flyer/FA0` folder of the Toaster files folder (beside the
//! source when there is no Flyer folder), and
//! an exported video beside the clip. ffmpeg does the decoding and
//! encoding: `COPPERLINE_FFMPEG`, the Toaster files folder, beside this
//! program, or the `PATH`.

use copperline::flyer_convert::{self, ImportOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: flyerclip import VIDEO [CLIP] [--hq5|--hq6] [--no-audio] [--seconds N]
       flyerclip export CLIP [VIDEO]
       flyerclip info CLIP...
       flyerclip FILE        (a clip is exported, anything else imported)

Converts between video files and NewTek Video Toaster Flyer clips. An
imported clip goes to the Flyer's drive FA0 (Flyer/FA0 in the Toaster files
folder) unless CLIP names a file; an exported video (MP4 unless VIDEO says
otherwise) goes beside the clip. Without an output name nothing is overwritten: a name in
use gets \" (2)\" added. Needs ffmpeg.";

fn is_clip(path: &Path) -> bool {
    std::fs::File::open(path)
        .and_then(|mut f| {
            let mut id = [0u8; 4];
            std::io::Read::read_exact(&mut f, &mut id)?;
            Ok(&id == b"CLIP")
        })
        .unwrap_or(false)
}

/// A clip name the Flyer accepts: at most 29 characters (31 with its
/// icon's `.I`), no `/` or `:`.
fn clip_name(stem: &str) -> String {
    let name: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '_'
            } else {
                c
            }
        })
        .take(29)
        .collect();
    if name.is_empty() {
        "Clip".into()
    } else {
        name
    }
}

fn default_clip_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("Clip");
    // The Flyer's first drive, FA0, is the folder `Flyer/FA0`.
    let flyer = copperline::paths::toaster_files_dir().join("Flyer");
    let dir = if flyer.is_dir() {
        flyer.join("FA0")
    } else {
        input.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    flyer_convert::unused_path(&dir.join(format!("{}.clip", clip_name(stem))))
}

fn progress(label: &'static str) -> impl FnMut(usize) {
    move |frames| {
        if frames % 30 == 0 {
            eprint!("\r{label}: {:.0} s", frames as f64 / 29.97);
        }
    }
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        return Err(USAGE.into());
    }
    // A file dropped on the program: decide by what it is.
    let verb = match args[0].as_str() {
        "import" | "export" | "info" => args.remove(0),
        _ if args.len() == 1 && is_clip(Path::new(&args[0])) => "export".into(),
        _ if args.len() == 1 => "import".into(),
        other => return Err(format!("unknown command {other:?}\n\n{USAGE}")),
    };
    let mut options = ImportOptions::default();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--hq5" => options.grade = flyerclip::Grade::Hq5,
            "--hq6" => options.grade = flyerclip::Grade::Hq6,
            "--no-audio" => options.audio = false,
            "--seconds" => {
                let v = it.next().ok_or("--seconds needs a number")?;
                options.max_seconds = Some(
                    v.parse()
                        .map_err(|_| format!("--seconds: bad number {v:?}"))?,
                );
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n\n{USAGE}")),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if verb == "info" {
        for f in &files {
            println!(
                "{}",
                flyer_convert::describe(f).map_err(|e| format!("{e:#}"))?
            );
        }
        return Ok(());
    }
    let input = files.first().ok_or(USAGE)?.clone();
    let ffmpeg = flyer_convert::find_ffmpeg(None).ok_or(
        "ffmpeg was not found: put ffmpeg.exe beside flyerclip.exe or in the Toaster files \
         folder, put it on the PATH, or set COPPERLINE_FFMPEG",
    )?;
    if verb == "import" {
        let output = files
            .get(1)
            .cloned()
            .unwrap_or_else(|| default_clip_path(&input));
        let report = flyer_convert::import(&ffmpeg, &input, &output, &options, progress("import"))
            .map_err(|e| format!("{e:#}"))?;
        eprintln!();
        println!(
            "{} -> {}: {} fields ({:.2} s), {} audio channel(s), {} bytes",
            input.display(),
            output.display(),
            report.fields,
            report.seconds,
            report.audio_channels,
            report.bytes
        );
        if report.degraded_fields + report.truncated_fields > 0 {
            println!(
                "{} field(s) needed lighter coding to fit the grade's budget, {} were cut \
                 short; --hq5 or --hq6 gives more room",
                report.degraded_fields, report.truncated_fields
            );
        }
    } else {
        let output = files
            .get(1)
            .cloned()
            .unwrap_or_else(|| flyer_convert::unused_path(&input.with_extension("mp4")));
        let report = flyer_convert::export(&ffmpeg, &input, &output, progress("export"))
            .map_err(|e| format!("{e:#}"))?;
        eprintln!();
        println!(
            "{} -> {}: {} frames ({:.2} s), {} audio channel(s)",
            input.display(),
            output.display(),
            report.frames,
            report.seconds,
            report.audio_channels
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    // A file dropped on flyerclip.exe gets a console window that would
    // close before the result could be read.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dropped = cfg!(windows)
        && args.len() == 1
        && !matches!(
            args[0].as_str(),
            "import" | "export" | "info" | "-h" | "--help"
        );
    let code = match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("flyerclip: {e}");
            ExitCode::FAILURE
        }
    };
    if dropped {
        eprintln!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}
