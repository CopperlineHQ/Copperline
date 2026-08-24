//! FS-UAE `Config.fs-uae` -> Copperline TOML. A different vocabulary from
//! WinUAE/Amiberry, not a rename: sizes are in KB rather than an
//! index/MB mix, and `amiga_model` is a database preset that implies a
//! chipset+CPU+RAM combination rather than each axis being spelled out
//! separately. An explicit `chipset`/`cpu_type`/`*_memory` always overrides
//! the preset, matching FS-UAE's own precedence.

use super::{annotate, set_str, table, MapOutcome};
use crate::parse::Entry;
use crate::report::ImportReport;
use std::collections::HashMap;
use toml_edit::DocumentMut;

/// (chipset revision, CPU model, chip RAM, fast RAM) for the common
/// `amiga_model` presets. Not exhaustive -- FS-UAE's database covers many
/// more variants (CD32, CDTV, A3000, A4000/040, ...); unrecognized presets
/// are flagged rather than guessed at.
fn model_preset(model: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    match model.to_ascii_uppercase().as_str() {
        "A500" => Some(("OCS", "68000", "512K", "0")),
        "A500+" | "A500PLUS" => Some(("ECS", "68000", "1M", "0")),
        "A600" => Some(("ECS", "68000", "1M", "0")),
        "A1200" => Some(("AGA", "68020", "2M", "0")),
        "A1200/020" => Some(("AGA", "68020", "2M", "8M")),
        "A2000" => Some(("ECS", "68000", "1M", "0")),
        "A3000" => Some(("ECS", "68030", "2M", "8M")),
        "A4000/030" => Some(("AGA", "68030", "2M", "8M")),
        "A4000/040" => Some(("AGA", "68040", "2M", "8M")),
        _ => None,
    }
}

pub fn map(entries: &[Entry]) -> MapOutcome {
    let mut doc = DocumentMut::new();
    let mut report = ImportReport::default();
    let mut seen: HashMap<&str, ()> = HashMap::new();
    let by_key = |k: &str| entries.iter().find(|e| e.key == k);

    // --- amiga_model preset, applied first so explicit keys below can
    // override individual axes of it -----------------------------------
    if let Some(e) = by_key("amiga_model") {
        seen.insert(&e.key, ());
        match model_preset(&e.value) {
            Some((chipset, cpu, chip_mem, fast_mem)) => {
                set_str(&mut doc, &["chipset"], "revision", chipset);
                set_str(&mut doc, &["cpu"], "model", cpu);
                set_str(&mut doc, &["memory"], "chip", chip_mem);
                if fast_mem != "0" {
                    set_str(&mut doc, &["memory"], "fast", fast_mem);
                }
                annotate(
                    &mut doc,
                    &["chipset"],
                    "revision",
                    &format!(
                        "derived from amiga_model={} -- verify against your source config",
                        e.value
                    ),
                );
            }
            None => report.approximated(
                &e.key,
                &e.value,
                "unrecognized amiga_model preset; set chipset/cpu/memory manually",
            ),
        }
    }

    // --- explicit overrides on top of (or instead of) the preset --------
    if let Some(e) = by_key("chipset") {
        seen.insert(&e.key, ());
        let revision = match e.value.to_ascii_lowercase().as_str() {
            "ocs" => Some("OCS"),
            "ecs" => Some("ECS"),
            "aga" => Some("AGA"),
            _ => None,
        };
        match revision {
            Some(rev) => set_str(&mut doc, &["chipset"], "revision", rev),
            None => report.unsupported(&e.key, &e.value, "unrecognized chipset value"),
        }
    }
    if let Some(e) = by_key("cpu_type") {
        seen.insert(&e.key, ());
        let digits: String = e.value.chars().take_while(|c| c.is_ascii_digit()).collect();
        let known = ["68000", "68010", "68020", "68030", "68040", "68060"];
        if known.contains(&digits.as_str()) {
            set_str(&mut doc, &["cpu"], "model", &digits);
        } else {
            report.unsupported(&e.key, &e.value, "unrecognized CPU model");
        }
    }

    // --- memory (FS-UAE: literal KB, not an index/MB mix) ----------------
    for (fsuae_key, section) in [
        ("chip_memory", "chip"),
        ("fast_memory", "fast"),
        ("slow_memory", "slow"),
    ] {
        if let Some(e) = by_key(fsuae_key) {
            seen.insert(&e.key, ());
            match e.value.trim().parse::<u64>() {
                Ok(kb) if kb > 0 => {
                    let size = if kb % 1024 == 0 {
                        format!("{}M", kb / 1024)
                    } else {
                        format!("{kb}K")
                    };
                    set_str(&mut doc, &["memory"], section, &size);
                }
                Ok(_) => {}
                Err(_) => report.unsupported(&e.key, &e.value, "expected a KB integer"),
            }
        }
    }

    // --- ROM ---------------------------------------------------------
    if let Some(e) = by_key("kickstart_file") {
        seen.insert(&e.key, ());
        doc["rom"] = toml_edit::value(e.value.as_str());
    }

    // --- NTSC/PAL ------------------------------------------------------
    if let Some(e) = by_key("ntsc_mode") {
        seen.insert(&e.key, ());
        let video = match e.value.trim() {
            "1" => Some("NTSC"),
            "0" => Some("PAL"),
            _ => None,
        };
        match video {
            Some(v) => set_str(&mut doc, &["chipset"], "video", v),
            None => report.unsupported(&e.key, &e.value, "expected 1 or 0"),
        }
    }

    // --- Floppies ----------------------------------------------------
    for (fsuae_key, drive) in [
        ("floppy_drive_0", "df0"),
        ("floppy_drive_1", "df1"),
        ("floppy_drive_2", "df2"),
        ("floppy_drive_3", "df3"),
    ] {
        if let Some(e) = by_key(fsuae_key) {
            seen.insert(&e.key, ());
            if !e.value.trim().is_empty() {
                set_str(&mut doc, &["floppy", drive], "path", &e.value);
            }
        }
    }
    if let Some(e) = by_key("floppy_drive_speed") {
        seen.insert(&e.key, ());
        match e.value.trim() {
            "turbo" => table(&mut doc, &["floppy"])["speed"] = toml_edit::value(0i64),
            other => match other.parse::<i64>() {
                Ok(speed) => table(&mut doc, &["floppy"])["speed"] = toml_edit::value(speed),
                Err(_) => report.unsupported(&e.key, &e.value, "unrecognized floppy speed"),
            },
        }
    }

    // --- hard drive directory mounts: FS-UAE's primary hard-drive path,
    // no WinUAE/Amiberry equivalent shape (a directory, not an image) --
    // map to Copperline's [[filesys]], but flag it since bootpri/volume
    // naming conventions differ enough to want a manual check.
    for i in 0..8 {
        let key = format!("hard_drive_{i}");
        if let Some(e) = by_key(&key) {
            seen.insert(&e.key, ());
            report.approximated(
                &e.key,
                &e.value,
                "FS-UAE hard drives may be image files or host directories; add a matching \
                 [ide]/[scsi]/[[filesys]] entry by hand",
            );
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
