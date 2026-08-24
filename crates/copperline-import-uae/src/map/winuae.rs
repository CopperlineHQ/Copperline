//! WinUAE/Amiberry `.uae` -> Copperline TOML. Amiberry is a WinUAE fork
//! that kept the same flat `key=value` vocabulary for the settings this
//! maps, so one mapper covers both; only WinUAE keys with no Amiberry
//! analogue (or vice versa) would need per-flavour branching, and none of
//! the core axes below have that split.

use super::{annotate, clamp_chip_mb, set_str, table, MapOutcome};
use crate::parse::Entry;
use crate::report::ImportReport;
use std::collections::HashMap;
use toml_edit::DocumentMut;

pub fn map(entries: &[Entry]) -> MapOutcome {
    let mut doc = DocumentMut::new();
    let mut report = ImportReport::default();
    let mut seen: HashMap<&str, ()> = HashMap::new();
    let by_key = |k: &str| entries.iter().find(|e| e.key == k);

    // --- chipset -----------------------------------------------------
    if let Some(e) = by_key("chipset") {
        seen.insert(&e.key, ());
        let revision = match e.value.to_ascii_lowercase().as_str() {
            "ocs" => Some("OCS"),
            "ecs" | "ecs_agnus" | "ecs_denise" => Some("ECS"),
            "aga" => Some("AGA"),
            _ => None,
        };
        match revision {
            Some(rev) => set_str(&mut doc, &["chipset"], "revision", rev),
            None => report.unsupported(
                &e.key,
                &e.value,
                "unrecognized chipset value; expected ocs/ecs/aga",
            ),
        }
    }

    // --- NTSC/PAL ------------------------------------------------------
    if let Some(e) = by_key("ntsc") {
        seen.insert(&e.key, ());
        let video = match e.value.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => "NTSC",
            "false" | "0" | "no" => "PAL",
            _ => {
                report.unsupported(&e.key, &e.value, "unrecognized boolean");
                ""
            }
        };
        if !video.is_empty() {
            set_str(&mut doc, &["chipset"], "video", video);
        }
    }

    // --- CPU -------------------------------------------------------------
    if let Some(e) = by_key("cpu_type") {
        seen.insert(&e.key, ());
        // WinUAE spells e.g. "68020", "68020i" (no MMU), "68030mmu",
        // "68040", "68060" -- Copperline only cares about the model digits.
        let digits: String = e.value.chars().take_while(|c| c.is_ascii_digit()).collect();
        let known = ["68000", "68010", "68020", "68030", "68040", "68060"];
        if known.contains(&digits.as_str()) {
            set_str(&mut doc, &["cpu"], "model", &digits);
        } else {
            report.unsupported(&e.key, &e.value, "unrecognized CPU model");
        }
    }
    if let Some(e) = by_key("cpu_compatible") {
        seen.insert(&e.key, ());
        report.approximated(
            &e.key,
            &e.value,
            "WinUAE's compatible/cycle-exact CPU core toggle has no direct Copperline knob; \
             Copperline's interpreter is always cycle-accurate",
        );
    }
    if let Some(e) = by_key("cpu_multiplier") {
        seen.insert(&e.key, ());
        report.unsupported(&e.key, &e.value, "no Copperline equivalent");
    }

    // --- Memory ------------------------------------------------------
    // WinUAE's memory-size keys have changed units across major versions
    // (an old doubling-index scheme vs. a literal size), so this is always
    // flagged for the user to double-check, whichever heuristic fires.
    for (uae_key, section, note) in [
        ("chipmem_size", "chip", "chip RAM"),
        ("fastmem_size", "fast", "fast RAM"),
        ("bogomem_size", "slow", "slow/Ranger RAM"),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            match guess_memory_size(&e.value) {
                Some(size) => {
                    let mut comment = format!(
                        "from {uae_key}={} as literal MB -- if this source config predates WinUAE 2.x, \
                         it may instead be a doubling index; verify against your source's actual {note} size",
                        e.value
                    );
                    let size = if section == "chip" {
                        let (clamped, clamp_note) = clamp_chip_mb(&size);
                        if let Some(clamp_note) = clamp_note {
                            comment = format!("{comment}; {clamp_note}");
                        }
                        clamped
                    } else {
                        size
                    };
                    set_str(&mut doc, &["memory"], section, &size);
                    annotate(&mut doc, &["memory"], section, &comment);
                }
                None => report.approximated(
                    &e.key,
                    &e.value,
                    format!("couldn't parse a {note} size from this value"),
                ),
            }
        }
    }

    // --- ROM -------------------------------------------------------------
    if let Some(e) = by_key("kickstart_rom_file") {
        seen.insert(&e.key, ());
        doc["rom"] = toml_edit::value(e.value.as_str());
    }

    // --- Floppies ----------------------------------------------------
    for (uae_key, drive) in [
        ("floppy0", "df0"),
        ("floppy1", "df1"),
        ("floppy2", "df2"),
        ("floppy3", "df3"),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            if !e.value.trim().is_empty() {
                set_str(&mut doc, &["floppy", drive], "path", &e.value);
            }
        }
    }
    if let Some(e) = by_key("floppy_speed") {
        seen.insert(&e.key, ());
        match e.value.parse::<i64>() {
            Ok(speed) => table(&mut doc, &["floppy"])["speed"] = toml_edit::value(speed),
            Err(_) => report.unsupported(&e.key, &e.value, "unrecognized floppy speed"),
        }
    }
    if let Some(e) = by_key("floppy_volume") {
        seen.insert(&e.key, ());
        match e.value.trim().parse::<i64>() {
            Ok(vol) if (0..=100).contains(&vol) => {
                table(&mut doc, &["audio"])["floppy_sounds_volume"] = toml_edit::value(vol);
            }
            _ => report.unsupported(&e.key, &e.value, "expected an integer 0-100"),
        }
    }

    // --- everything else --------------------------------------------
    for e in entries {
        if seen.contains_key(e.key.as_str()) {
            continue;
        }
        report.unsupported(
            &e.key,
            &e.value,
            "not yet recognized by this converter (may still have a Copperline equivalent)",
        );
    }

    MapOutcome { doc, report }
}

/// WinUAE memory sizes have been spelled at least two ways across versions:
/// current WinUAE/Amiberry (2.x+, effectively every `.uae` file in the
/// wild today) writes a literal megabyte count; pre-2.x WinUAE wrote a
/// doubling index from 256K instead (0=256K, 1=512K, 2=1M, 3=2M...). The
/// literal reading is assumed since it matches the format nearly everyone
/// actually has; callers still flag the result so a config from a very old
/// WinUAE install gets a chance to be caught and fixed by hand.
fn guess_memory_size(value: &str) -> Option<String> {
    let n: u64 = value.trim().parse().ok()?;
    Some(format!("{n}M"))
}
