//! FS-UAE `Config.fs-uae` -> Copperline TOML. A different vocabulary from
//! WinUAE/Amiberry, not a rename: sizes are in KB rather than an
//! index/MB mix, and `amiga_model` is a database preset that implies a
//! chipset+CPU+RAM combination rather than each axis being spelled out
//! separately. An explicit `chipset`/`cpu_type`/`*_memory` always overrides
//! the preset, matching FS-UAE's own precedence.

use super::{annotate, clamp_chip_mb, set_str, table, MapOutcome};
use crate::parse::Entry;
use crate::report::ImportReport;
use std::collections::HashMap;
use toml_edit::DocumentMut;

/// (chipset revision, CPU model, chip RAM, fast RAM) for the common
/// `amiga_model` presets. Not exhaustive -- FS-UAE's database covers many
/// more variants (CD32, CDTV, A3000, A4000/040, ...); unrecognized presets
/// are flagged rather than guessed at.
fn model_preset(
    model: &str,
) -> Option<(
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
)> {
    match model.to_ascii_uppercase().as_str() {
        "A500" => Some(("OCS", "68000", "512K", "0", "A500")),
        "A500+" | "A500PLUS" => Some(("ECS", "68000", "1M", "0", "A500Plus")),
        "A600" => Some(("ECS", "68000", "1M", "0", "A600")),
        "A1200" => Some(("AGA", "68020", "2M", "0", "A1200")),
        "A1200/020" => Some(("AGA", "68020", "2M", "8M", "A1200")),
        // Copperline has no A2000 profile; the A500 is the same chipset
        // generation and the closest machine it does model.
        "A2000" => Some(("ECS", "68000", "1M", "0", "A500")),
        "A3000" => Some(("ECS", "68030", "2M", "8M", "A3000")),
        "A4000/030" => Some(("AGA", "68030", "2M", "8M", "A4000")),
        "A4000/040" => Some(("AGA", "68040", "2M", "8M", "A4000")),
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
    let mut machine_profile = "";
    if let Some(e) = by_key("amiga_model") {
        seen.insert(&e.key, ());
        match model_preset(&e.value) {
            Some((chipset, cpu, chip_mem, fast_mem, profile)) => {
                // The profile carries the machine's own wiring (the IDE
                // port an A1200/A600/A4000 has, the RTC socket, and the
                // rest); without it a `hard_drive_N` mapped onto [ide]
                // below has no controller to attach to. The explicit
                // chipset/CPU/memory set here still override whatever the
                // profile would have supplied.
                set_str(&mut doc, &["machine"], "profile", profile);
                machine_profile = profile;
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
                    let clamp_note = if section == "chip" {
                        let (clamped, clamp_note) = clamp_chip_mb(&size);
                        set_str(&mut doc, &["memory"], section, &clamped);
                        clamp_note
                    } else {
                        set_str(&mut doc, &["memory"], section, &size);
                        None
                    };
                    if let Some(clamp_note) = clamp_note {
                        annotate(&mut doc, &["memory"], section, &clamp_note);
                    }
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
    // `floppy_image_N` is FS-UAE's swap list -- the stack of disks a
    // multi-floppy release ships, cycled from the GUI -- which is exactly
    // what Copperline's `[floppy.df0] paths` playlist is for. Collected in
    // numeric order rather than file order, since a config may list them
    // any way round and the sequence is the whole point. Where a
    // `floppy_drive_0` is also set, Copperline treats it as the first
    // entry followed by this list, so both keys can coexist.
    let mut playlist: Vec<(u32, &str)> = entries
        .iter()
        .filter_map(|e| {
            let n: u32 = e.key.strip_prefix("floppy_image_")?.parse().ok()?;
            let path = e.value.trim();
            (!path.is_empty()).then_some((n, path))
        })
        .collect();
    playlist.sort_by_key(|(n, _)| *n);
    if !playlist.is_empty() {
        for e in entries.iter() {
            if e.key.starts_with("floppy_image_") {
                seen.insert(&e.key, ());
            }
        }
        let mut arr = toml_edit::Array::new();
        for (_, path) in &playlist {
            arr.push(*path);
        }
        table(&mut doc, &["floppy", "df0"])["paths"] = toml_edit::value(arr);
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

    // --- Zorro III RAM (FS-UAE counts KB, like its other memory keys) ---
    if let Some(e) = by_key("zorro_iii_memory") {
        seen.insert(&e.key, ());
        match e.value.trim().parse::<u64>() {
            Ok(0) => {}
            Ok(kb) if kb.is_multiple_of(1024) => {
                set_str(&mut doc, &["memory"], "z3", &format!("{}M", kb / 1024))
            }
            Ok(kb) => set_str(&mut doc, &["memory"], "z3", &format!("{kb}K")),
            Err(_) => report.unsupported(&e.key, &e.value, "expected a KB integer"),
        }
    }

    // --- Hard drives ----------------------------------------------------
    // `hard_drive_N` is a path, with an optional `hard_drive_N_controller`
    // saying where it hangs. Copperline splits those across sections, so
    // the controller decides which one: `scsi` becomes a `[scsi]` unit,
    // anything else (including the unset default) an `[ide]` master/slave
    // pair. An `.hdf`-style image is what those sections take; a host
    // *directory* is a different thing entirely -- Copperline mounts those
    // as `[[filesys]]` -- and a raw device node is a third
    // (`[[host_disk]]`, deliberately not auto-configured: handing a real
    // disk to a guest should be an explicit choice, not an import
    // side-effect). Both of the latter are flagged rather than guessed.
    for i in 0..8u32 {
        let key = format!("hard_drive_{i}");
        let Some(e) = by_key(&key) else { continue };
        seen.insert(&e.key, ());
        let path = e.value.trim();
        if path.is_empty() {
            continue;
        }
        let controller = by_key(&format!("hard_drive_{i}_controller"));
        if let Some(c) = controller {
            seen.insert(&c.key, ());
        }
        // `hard_drive_N_type` (rdb, etc.) describes the image's own layout,
        // which Copperline works out from the file itself.
        if let Some(t) = by_key(&format!("hard_drive_{i}_type")) {
            seen.insert(&t.key, ());
        }
        if path.starts_with("/dev/") {
            report.approximated(
                &e.key,
                &e.value,
                "a raw host device; Copperline can do this with [[host_disk]], but handing a \
                 real disk to the guest is left as an explicit choice rather than an import",
            );
            continue;
        }
        if !path.contains('.') {
            report.approximated(
                &e.key,
                &e.value,
                "looks like a host directory rather than a drive image; add it as a \
                 [[filesys]] mount (with the volume name you want) by hand",
            );
            continue;
        }
        // Only the A600/A1200/A4000 have an onboard IDE port. With no
        // controller named, anything else has to go on SCSI (a Zorro
        // adapter, or the A3000's own motherboard SDMAC) -- defaulting to
        // IDE there produces a machine Copperline rightly refuses to
        // build, since there is no port for the drive to hang off.
        let has_onboard_ide = matches!(machine_profile, "A600" | "A1200" | "A4000");
        let scsi = match controller {
            Some(c) => c.value.trim().to_ascii_lowercase().starts_with("scsi"),
            None => !has_onboard_ide,
        };
        if scsi {
            if i < 7 {
                set_str(&mut doc, &["scsi"], &format!("unit{i}"), path);
            } else {
                report.unsupported(&e.key, &e.value, "Copperline's [scsi] has units 0-6");
            }
        } else {
            match i {
                0 => set_str(&mut doc, &["ide"], "master", path),
                1 => set_str(&mut doc, &["ide"], "slave", path),
                _ => report.approximated(
                    &e.key,
                    &e.value,
                    "Copperline's [ide] has only master and slave; put the rest on [scsi] or \
                     [lide] by hand",
                ),
            }
        }
    }

    // FS-UAE expands `$HOME`, `$BASE`, `$CONFIG` and friends inside paths;
    // Copperline takes paths literally, so any that survived the mapping
    // above would point at a directory named "$HOME". Reported per key so
    // it is obvious which ones need rewriting to absolute paths, rather
    // than failing later as a missing file.
    for e in entries
        .iter()
        .filter(|e| seen.contains_key(e.key.as_str()) && e.value.contains('$'))
    {
        report.approximated(
            &e.key,
            &e.value,
            "FS-UAE path variables ($HOME, $BASE, ...) are expanded by FS-UAE, not by \
             Copperline; rewrite this as an absolute path",
        );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(text: &str) -> String {
        let entries = crate::parse::parse(text);
        map(&entries).doc.to_string()
    }

    #[test]
    fn floppy_images_become_a_swap_playlist_in_numeric_order() {
        // Deliberately out of order, and past 9, so a plain string sort
        // would get it wrong: the sequence is the whole point of a
        // multi-disk release.
        let out = convert(
            "floppy_image_10 = ten.adf\n\
             floppy_image_2 = two.adf\n\
             floppy_image_1 = one.adf\n",
        );
        let paths = out
            .lines()
            .find(|l| l.starts_with("paths ="))
            .expect("a playlist");
        assert_eq!(
            paths, r#"paths = ["one.adf", "two.adf", "ten.adf"]"#,
            "{out}"
        );
        // The parent exists only to hold [floppy.df0]; a bare [floppy]
        // header with nothing under it would be noise.
        assert!(!out.contains("[floppy]\n"), "{out}");
    }

    #[test]
    fn hard_drives_follow_their_controller() {
        let out = convert("hard_drive_0 = os.hdf\nhard_drive_0_controller = ide0\n");
        assert!(out.contains(r#"master = "os.hdf""#), "{out}");

        let out = convert("hard_drive_0 = os.hdf\nhard_drive_0_controller = scsi\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
    }

    #[test]
    fn an_unnamed_controller_follows_the_machine_s_own_ports() {
        // A1200 has an onboard IDE port, so an unqualified drive hangs off
        // it...
        let out = convert("amiga_model = A1200\nhard_drive_1 = data.hdf\n");
        assert!(out.contains(r#"slave = "data.hdf""#), "{out}");

        // ...but an A3000 has SCSI and no IDE, and defaulting to IDE there
        // builds a machine with no port for the drive, which Copperline
        // rightly refuses. Same for an unknown/absent model: assuming an
        // IDE port is the guess that fails.
        let out = convert("amiga_model = A3000\nhard_drive_0 = os.hdf\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
        assert!(!out.contains("master ="), "{out}");

        let out = convert("hard_drive_0 = os.hdf\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
    }

    #[test]
    fn directories_and_raw_devices_are_flagged_rather_than_guessed() {
        // A host directory is a [[filesys]] mount, not a drive image, and a
        // raw device is a real disk -- neither should be auto-attached.
        let entries = crate::parse::parse("hard_drive_0 = /host/workbench\n");
        let out = map(&entries);
        assert!(!out.doc.to_string().contains("master ="), "{}", out.doc);
        assert_eq!(out.report.flagged.len(), 1);

        let entries = crate::parse::parse("hard_drive_0 = /dev/disk10\n");
        let out = map(&entries);
        assert!(!out.doc.to_string().contains("master ="), "{}", out.doc);
        assert_eq!(out.report.flagged.len(), 1);
    }

    #[test]
    fn fsuae_path_variables_are_flagged_not_taken_literally() {
        // FS-UAE expands these itself; left alone they would point at a
        // directory actually named "$HOME".
        let entries = crate::parse::parse("floppy_drive_0 = $HOME/disks/wb.adf\n");
        let out = map(&entries);
        assert_eq!(out.report.flagged.len(), 1);
        assert!(out.report.flagged[0].note.contains("path variables"));
    }

    #[test]
    fn zorro_iii_memory_is_counted_in_kilobytes() {
        assert!(convert("zorro_iii_memory = 131072\n").contains(r#"z3 = "128M""#));
        assert!(!convert("zorro_iii_memory = 0\n").contains("z3 ="));
    }
}
