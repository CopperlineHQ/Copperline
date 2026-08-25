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

/// Where a drive with no controller of its own can go on this machine.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Port {
    /// The A600/A1200/A4000's own Gayle/A4000 IDE port.
    Ide,
    /// The A3000's motherboard SCSI: no board and no boot ROM needed.
    Scsi,
    /// Neither -- IDE is still the closest thing, but the machine needs
    /// changing for it to exist.
    IdeMissing,
}

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
    // Last occurrence wins, as FS-UAE itself applies them: a key restated
    // further down the file is the one that took effect.
    let by_key = |k: &str| entries.iter().rev().find(|e| e.key == k);

    // --- amiga_model preset, applied first so explicit keys below can
    // override individual axes of it -----------------------------------
    let mut machine_profile = "";
    if by_key("amiga_model").is_none() {
        report.note(
            "the source config named no machine model (amiga_model), so the machine is whatever Copperline defaults to -- currently a stock A500 (68000 at 7.09MHz, 512K chip plus 512K slow, ECS Agnus with OCS Denise, PAL). Set [machine] profile if you wanted something else; the CPU/chipset/memory keys that did translate still override it either way.",
        );
    }
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
    // A restated index replaces the earlier line rather than adding a
    // second disk to the playlist, the same last-wins rule the rest of the
    // mapper follows. The sort is stable, so reversing it puts each index's
    // last line first and `dedup_by_key` (which keeps the first of a run)
    // drops the superseded ones.
    playlist.sort_by_key(|(n, _)| *n);
    playlist.reverse();
    playlist.dedup_by_key(|(n, _)| *n);
    playlist.reverse();
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
        // A drive image is named by its extension, which lives in the last
        // path component: testing the whole path takes a dotted *parent*
        // (`~/.fs-uae/hd/System`, `games/v1.2/wb`) for an image and hands a
        // directory to [ide], where it fails to open as an HDF instead of
        // being flagged for [[filesys]].
        let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if !leaf.contains('.') {
            report.approximated(
                &e.key,
                &e.value,
                "looks like a host directory rather than a drive image; add it as a \
                 [[filesys]] mount (with the volume name you want) by hand",
            );
            continue;
        }
        // An unnamed controller is not "whichever bus this machine has":
        // FS-UAE's default is its own `uae` virtual controller
        // (uaehf.device), served by the emulator rather than by hardware
        // the Amiga can see. Copperline has no counterpart, so -- as with
        // WinUAE's `uae` hardfiles -- the drive goes on the real IDE port,
        // which is a substitution worth saying rather than a like-for-like
        // move. Inventing a SCSI adapter instead would fit hardware the
        // source never had.
        let named = controller.map(|c| c.value.trim().to_ascii_lowercase());
        let virtual_controller = matches!(named.as_deref(), None | Some("") | Some("uae"));
        // Where the substitution lands depends on what the machine
        // actually has: the A600/A1200/A4000's own IDE port, or the
        // A3000's motherboard SCSI (which needs no board and no boot ROM).
        // A machine with neither goes on IDE anyway rather than onto a
        // Zorro SCSI board the source never fitted -- that would need a
        // ROM image as well, and fail validation for want of it.
        let onboard = match machine_profile {
            "A600" | "A1200" | "A4000" => Port::Ide,
            "A3000" => Port::Scsi,
            _ => Port::IdeMissing,
        };
        if virtual_controller {
            let port = match onboard {
                Port::Ide => ", which puts the image behind the Kickstart's own size limits",
                Port::Scsi => "'s motherboard SCSI",
                Port::IdeMissing => {
                    ", which this machine does not have -- set [machine] profile to an \
                     A600/A1200/A4000 (or move the drive to [scsi] or [lide] by hand)"
                }
            };
            report.approximated(
                &e.key,
                &e.value,
                format!(
                    "FS-UAE served this drive through its own uaehf.device virtual controller, \
                     which Copperline has no equivalent for; attached to the machine{port}"
                ),
            );
        }
        let scsi = match &named {
            Some(c) if !c.is_empty() => c.starts_with("scsi"),
            _ => onboard == Port::Scsi,
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
    fn an_unnamed_controller_is_the_virtual_one_and_says_so() {
        // FS-UAE's default is its own uaehf.device, not whichever bus the
        // machine has, so every one of these is a substitution and has to
        // be reported as one -- but it still has to land somewhere the
        // machine can actually carry it.
        let entries = crate::parse::parse("amiga_model = A1200\nhard_drive_1 = data.hdf\n");
        let out = map(&entries);
        assert!(
            out.doc.to_string().contains(r#"slave = "data.hdf""#),
            "{}",
            out.doc
        );
        assert!(out.report.flagged[0].note.contains("uaehf.device"));

        // The A3000's motherboard SCSI needs no board and no boot ROM, so
        // that is where its drives go; IDE there would be a port the
        // machine has not got.
        let out = convert("amiga_model = A3000\nhard_drive_0 = os.hdf\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
        assert!(!out.contains("master ="), "{out}");

        // A machine with neither: IDE anyway (the closest real port), with
        // the machine change named. Putting it on [scsi] instead would fit
        // a Zorro board the source never had and fail validation for want
        // of its ROM.
        let entries = crate::parse::parse("hard_drive_0 = os.hdf\n");
        let out = map(&entries);
        assert!(
            out.doc.to_string().contains(r#"master = "os.hdf""#),
            "{}",
            out.doc
        );
        assert!(out.report.flagged[0].note.contains("[machine] profile"));
    }

    #[test]
    fn an_explicit_controller_still_wins() {
        let out =
            convert("amiga_model = A1200\nhard_drive_0 = os.hdf\nhard_drive_0_controller = scsi\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
        assert!(!out.contains("master ="), "{out}");
    }

    #[test]
    fn a_restated_floppy_image_replaces_its_earlier_line() {
        // Last-wins, as everywhere else: a repeated index is a correction,
        // not a second disk in the swap list.
        let out =
            convert("floppy_image_0 = a.adf\nfloppy_image_1 = b.adf\nfloppy_image_0 = c.adf\n");
        assert!(out.contains(r#"paths = ["c.adf", "b.adf"]"#), "{out}");
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

        // The extension lives in the last component: a dotted parent
        // directory (FS-UAE's own ~/.fs-uae, a versioned folder) does not
        // make the mount a drive image.
        let entries = crate::parse::parse("hard_drive_0 = /home/me/.fs-uae/hd/System\n");
        let out = map(&entries);
        assert!(!out.doc.to_string().contains("master ="), "{}", out.doc);
        assert_eq!(out.report.flagged.len(), 1);

        // ...and a real image under such a parent still is one.
        let entries = crate::parse::parse("hard_drive_0 = /home/me/.fs-uae/hd/System.hdf\n");
        let out = map(&entries);
        assert!(out.doc.to_string().contains("master ="), "{}", out.doc);
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

#[cfg(test)]
mod note_tests {
    use super::*;

    #[test]
    fn a_config_naming_no_model_says_so_rather_than_stamping_one() {
        // Copperline's own default machine already *is* a stock A500, so
        // writing `profile = "A500"` here would change nothing while
        // making a guess look like something the source config asserted.
        // Saying it in the report instead is honest and actionable.
        let entries = crate::parse::parse("chip_memory = 512\n");
        let out = map(&entries);
        assert!(!out.doc.to_string().contains("profile ="), "{}", out.doc);
        assert_eq!(out.report.notes.len(), 1);
        assert!(out.report.notes[0].contains("no machine model"));

        // ...and a config that does name one gets no such note.
        let entries = crate::parse::parse("amiga_model = A1200\n");
        let out = map(&entries);
        assert!(out.report.notes.is_empty());
        assert!(out.doc.to_string().contains(r#"profile = "A1200""#));
    }
}
