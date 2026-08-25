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

#[path = "import_uae/map/mod.rs"]
mod map;
#[path = "import_uae/parse.rs"]
mod parse;
#[path = "import_uae/report.rs"]
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
    let mut outcome = map::map(args.format, &entries, &args.input);
    // A line the tokenizer could make nothing of is still something the
    // source said; it belongs in the report like any other untranslated
    // content, not on the floor.
    for line in parse::unreadable_lines(&text) {
        outcome.report.note(format!(
            "this line of the source config is not a `key=value` setting and was not read: {line}"
        ));
    }

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
        // Validation opens the media the config names, so importing on a
        // host that doesn't hold the source machine's images fails on
        // files that are merely elsewhere. That is not a mapping bug, and
        // warning about it on every real import trains the reader to
        // ignore the warning that does mean one -- so complaints that name
        // an absent file are separated out from the rest.
        let absent = absent_media(&outcome.doc, &args.input);
        let text = format!("{e:#}");
        let mut unexplained: Vec<&str> = text
            .lines()
            .filter(|line| line.trim_start().starts_with("- "))
            .filter(|line| !absent.iter().any(|p| line.contains(p.as_str())))
            .collect();
        // A single-error message has no "  - " lines to filter; it is
        // itself the complaint.
        let single = unexplained.is_empty() && !text.contains("\n  - ");
        if single && !absent.iter().any(|p| text.contains(p.as_str())) {
            unexplained.push(text.as_str());
        }
        if !unexplained.is_empty() {
            eprintln!("warning: generated config did not validate cleanly:");
            for line in unexplained {
                eprintln!("  {}", line.trim_start().trim_start_matches("- "));
            }
            eprintln!("it has still been written -- fix the reported settings by hand.");
        } else {
            // Media checks stop at the first missing file, so this is
            // "checking got no further", not "everything else is fine".
            eprintln!(
                "note: {} file(s) the source config names are not on this host, so checking \
                 stopped there; copy them across (or fix the paths) and re-run to have the \
                 rest of the config checked.",
                absent.len()
            );
        }
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
    // Only trailer entries are counted; a setting that came across with an
    // inline `#` comment on its own line is not one of these, so the
    // wording points at both rather than implying the count covers every
    // approximation in the file.
    eprintln!(
        "wrote {} ({} setting(s) approximated, {} not translated -- see the trailing comment, \
         and the inline # comments for settings that changed shape on the way in)",
        args.output.display(),
        approximated,
        unsupported
    );

    Ok(())
}

/// Every host path the generated config names that this machine does not
/// have, resolved the way the emulator that wrote the source would (bare
/// names live next to the source config, or in its install root's media
/// folders). Used only to tell "the image is on the other machine" apart
/// from a real complaint about the generated file.
fn absent_media(doc: &toml_edit::DocumentMut, source: &std::path::Path) -> Vec<String> {
    fn paths(item: &toml_edit::Item, out: &mut Vec<String>) {
        match item {
            // A drive is either a bare path or a table with one.
            toml_edit::Item::Value(toml_edit::Value::String(s)) => out.push(s.value().clone()),
            toml_edit::Item::Value(toml_edit::Value::InlineTable(t)) => {
                if let Some(p) = t.get("path").and_then(|v| v.as_str()) {
                    out.push(p.to_string());
                }
            }
            toml_edit::Item::Table(t) => {
                if let Some(p) = t.get("path").and_then(|v| v.as_str()) {
                    out.push(p.to_string());
                }
                // A floppy drive can hold a swap playlist instead of (or
                // as well as) a single image; every entry is media this
                // host may or may not have.
                if let Some(list) = t.get("paths").and_then(|v| v.as_array()) {
                    out.extend(list.iter().filter_map(|v| v.as_str()).map(str::to_string));
                }
            }
            _ => {}
        }
    }

    let mut named = Vec::new();
    if let Some(rom) = doc.get("rom") {
        paths(rom, &mut named);
    }
    // The tables holding host paths, and the keys within them that carry
    // one; `[floppy]`'s and `[[filesys]]`'s are nested a level deeper.
    for (section, keys) in [
        ("cd", &["image"][..]),
        ("ide", &["master", "slave"]),
        (
            "scsi",
            &[
                "rom", "rom_odd", "unit0", "unit1", "unit2", "unit3", "unit4", "unit5", "unit6",
            ],
        ),
        (
            "lide",
            &["rom", "rom_bank2", "drive0", "drive1", "drive2", "drive3"],
        ),
    ] {
        let Some(table) = doc.get(section) else {
            continue;
        };
        for key in keys {
            if let Some(item) = table.get(key) {
                paths(item, &mut named);
            }
        }
    }
    if let Some(floppy) = doc.get("floppy") {
        for drive in ["df0", "df1", "df2", "df3"] {
            if let Some(d) = floppy.get(drive) {
                paths(d, &mut named);
            }
        }
    }
    if let Some(toml_edit::Item::ArrayOfTables(mounts)) = doc.get("filesys") {
        for mount in mounts.iter() {
            if let Some(p) = mount.get("path").and_then(|v| v.as_str()) {
                named.push(p.to_string());
            }
        }
    }

    named
        .into_iter()
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let given = std::path::Path::new(p);
            !given.exists() && map::resolve_media_path(source, p).is_none()
        })
        .collect()
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
