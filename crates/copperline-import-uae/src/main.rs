//! Converts a WinUAE, Amiberry, or FS-UAE config file into Copperline TOML.
//!
//! Usage: copperline-import-uae --from winuae|amiberry|fsuae --in FILE --out FILE
//!
//! The generated file is validated the same way Copperline itself would
//! load it (`RawConfig::parse` + `Config::try_from`) before being written,
//! and every source setting that didn't cleanly translate is listed in a
//! trailing comment block, split into settings that were approximated
//! (translated, but worth double-checking) and settings with no Copperline
//! equivalent at all. Nothing from the source file is silently dropped.

mod map;
mod parse;
mod report;

use copperline::config::{Config, RawConfig};
use map::SourceFormat;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    format: SourceFormat,
    input: PathBuf,
    output: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut format = None;
    let mut input = None;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--from" => {
                let value = args.next().ok_or("--from needs a value")?;
                format = Some(SourceFormat::parse_name(&value).ok_or_else(|| {
                    format!("--from {value}: expected winuae, amiberry, or fsuae")
                })?);
            }
            "--in" => input = Some(PathBuf::from(args.next().ok_or("--in needs a value")?)),
            "--out" => output = Some(PathBuf::from(args.next().ok_or("--out needs a value")?)),
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(Args {
        format: format.ok_or("missing --from winuae|amiberry|fsuae")?,
        input: input.ok_or("missing --in FILE")?,
        output: output.ok_or("missing --out FILE")?,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args().map_err(|e| {
        format!(
            "{e}\n\nusage: copperline-import-uae --from winuae|amiberry|fsuae --in FILE --out FILE"
        )
    })?;

    let text = std::fs::read_to_string(&args.input)
        .map_err(|e| format!("reading {}: {e}", args.input.display()))?;
    let entries = parse::parse(&text);
    let outcome = map::map(args.format, &entries);

    let mut output = outcome.doc.to_string();
    let trailer = outcome.report.trailer_comment();
    if !trailer.is_empty() {
        output.push('\n');
        output.push_str(&trailer);
    }

    // Validate exactly the way Copperline itself would load this file,
    // so a bug in the mapper is caught here rather than at the user's
    // next `copperline --config`.
    let raw = RawConfig::parse(&output)
        .map_err(|e| format!("generated config is invalid TOML: {e:#}"))?;
    if let Err(e) = Config::try_from(raw) {
        eprintln!("warning: generated config did not validate cleanly: {e}");
        eprintln!("it has still been written -- fix the reported settings by hand.");
    }

    std::fs::write(&args.output, &output)
        .map_err(|e| format!("writing {}: {e}", args.output.display()))?;

    let approximated = outcome
        .report
        .flagged
        .iter()
        .filter(|f| f.bucket == report::Bucket::Approximated)
        .count();
    let unsupported = outcome.report.flagged.len() - approximated;
    eprintln!(
        "wrote {} ({} setting(s) approximated, {} not translated -- see the trailing comment)",
        args.output.display(),
        approximated,
        unsupported
    );

    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
