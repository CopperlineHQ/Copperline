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

    // --- Amiberry file-dialog starting directories -----------------------
    // WinUAE-only files never carry these (they're Amiberry GUI settings),
    // so they're a no-op there.
    for (amiberry_key, paths_key) in [
        ("amiberry.rom_path", "roms"),
        ("amiberry.floppy_path", "floppies"),
    ] {
        if let Some(e) = by_key(amiberry_key) {
            seen.insert(&e.key, ());
            if !e.value.trim().is_empty() {
                set_str(&mut doc, &["paths"], paths_key, &e.value);
            }
        }
    }
    if let Some(e) = by_key("amiberry.soundcardname") {
        seen.insert(&e.key, ());
        if !e.value.trim().is_empty() {
            set_str(&mut doc, &["audio"], "output_device", &e.value);
            annotate(
                &mut doc,
                &["audio"],
                "output_device",
                "from amiberry.soundcardname -- Amiberry and Copperline enumerate host audio \
                 devices differently, so this name may not match exactly; verify it selects \
                 the intended device",
            );
        }
    }

    // --- Amiberry scaling_method: -1 Auto, 0 Nearest, 1 Linear, 2
    // Integer, 3 Stretch (BlitterStudio/amiberry src/osdep/imgui/display.cpp).
    // Copperline only has two [display] scaling modes: "smooth" (filtered,
    // aspect-preserving) and "integer" (pixel-perfect). 1 (Linear) and 2
    // (Integer) match those exactly; 0 (Nearest) and -1 (Auto) have no
    // exact equivalent -- Copperline's "smooth" is always filtered, and it
    // has no per-mode auto-integer switch -- so those are approximated to
    // the closest behavior and flagged; 3 (Stretch) ignores aspect ratio
    // entirely, which nothing in Copperline does, so it's left unset and
    // flagged unsupported rather than silently picking something else.
    if let Some(e) = by_key("amiberry.scaling_method") {
        seen.insert(&e.key, ());
        match e.value.trim() {
            "2" => set_str(&mut doc, &["display"], "scaling", "integer"),
            "1" => set_str(&mut doc, &["display"], "scaling", "smooth"),
            "0" => {
                set_str(&mut doc, &["display"], "scaling", "smooth");
                annotate(
                    &mut doc,
                    &["display"],
                    "scaling",
                    "from amiberry.scaling_method=0 (Nearest) -- Copperline's \"smooth\" mode \
                     is always filtered; there's no non-integer nearest-neighbor mode",
                );
            }
            "-1" => {
                set_str(&mut doc, &["display"], "scaling", "smooth");
                annotate(
                    &mut doc,
                    &["display"],
                    "scaling",
                    "from amiberry.scaling_method=-1 (Auto) -- Copperline has no equivalent \
                     per-mode auto-integer switch; verify \"smooth\" gives the look you want",
                );
            }
            "3" => report.unsupported(
                &e.key,
                &e.value,
                "Stretch (ignores aspect ratio) has no Copperline equivalent",
            ),
            _ => report.unsupported(&e.key, &e.value, "unrecognized scaling_method value"),
        }
    }

    // --- RTC / battery clock --------------------------------------------
    // `[machine] battmem` backs only the RP5C01's battery RAM (the
    // A3000/A4000 part) -- the MSM6242 (the common A500+/A600/A1200 part)
    // has no battery RAM of its own in Copperline's model, so rtc_file is
    // only translated when cs_rtc says RP5C01 is actually fitted.
    let mut rtc_chip_is_rp5c01 = false;
    if let Some(e) = by_key("cs_rtc") {
        seen.insert(&e.key, ());
        let lower = e.value.trim().to_ascii_lowercase();
        if lower == "none" || lower == "0" {
            table(&mut doc, &["machine"])["rtc"] = toml_edit::value(false);
        } else if lower.starts_with("msm6242") {
            table(&mut doc, &["machine"])["rtc"] = toml_edit::value(true);
            set_str(&mut doc, &["machine"], "rtc_chip", "MSM6242");
        } else if lower.starts_with("rp5c01") {
            table(&mut doc, &["machine"])["rtc"] = toml_edit::value(true);
            set_str(&mut doc, &["machine"], "rtc_chip", "RP5C01");
            rtc_chip_is_rp5c01 = true;
        } else {
            report.unsupported(&e.key, &e.value, "unrecognized RTC chip");
        }
    }
    if let Some(e) = by_key("rtc_file") {
        seen.insert(&e.key, ());
        if e.value.trim().is_empty() {
            // nothing to translate
        } else if rtc_chip_is_rp5c01 {
            set_str(&mut doc, &["machine"], "battmem", &e.value);
        } else {
            report.unsupported(
                &e.key,
                &e.value,
                "Copperline's battmem only backs the RP5C01 (A3000/A4000); the MSM6242 \
                 (the common case) has no battery RAM of its own to restore this into",
            );
        }
    }

    // --- FPU ------------------------------------------------------------
    if let Some(e) = by_key("fpu_model") {
        seen.insert(&e.key, ());
        let lower = e.value.trim().to_ascii_lowercase();
        let has_fpu = !(lower.is_empty() || lower == "0" || lower == "none");
        table(&mut doc, &["cpu"])["fpu"] = toml_edit::value(has_fpu);
    }

    // --- Audio ------------------------------------------------------
    if let Some(e) = by_key("sound_stereo_separation") {
        seen.insert(&e.key, ());
        match e.value.trim().parse::<i64>() {
            Ok(sep) => table(&mut doc, &["audio"])["stereo_separation"] = toml_edit::value(sep),
            Err(_) => report.unsupported(&e.key, &e.value, "expected an integer"),
        }
    }

    // --- Display ----------------------------------------------------
    if let Some(e) = by_key("gfx_fullscreen_amiga") {
        seen.insert(&e.key, ());
        let lower = e.value.trim().to_ascii_lowercase();
        match lower.as_str() {
            "fullscreen" | "fullwindow" => {
                table(&mut doc, &["display"])["full_screen"] = toml_edit::value(true)
            }
            "window" => table(&mut doc, &["display"])["full_screen"] = toml_edit::value(false),
            _ => report.unsupported(&e.key, &e.value, "unrecognized fullscreen mode"),
        }
    }
    if let Some(e) = by_key("show_leds") {
        seen.insert(&e.key, ());
        match parse_bool(&e.value) {
            Some(on) => table(&mut doc, &["display"])["status_bar"] = toml_edit::value(on),
            None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
        }
    }

    // --- Floppies (write-protect, drive count) ---------------------------
    for (uae_key, drive) in [
        ("floppy0wp", "df0"),
        ("floppy1wp", "df1"),
        ("floppy2wp", "df2"),
        ("floppy3wp", "df3"),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            match parse_bool(&e.value) {
                Some(on) => {
                    table(&mut doc, &["floppy", drive])["write_protected"] = toml_edit::value(on)
                }
                None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
            }
        }
    }
    if let Some(e) = by_key("nr_floppies") {
        seen.insert(&e.key, ());
        match e.value.trim().parse::<i64>() {
            Ok(n) if (1..=4).contains(&n) => {
                table(&mut doc, &["floppy"])["drives"] = toml_edit::value(n)
            }
            _ => report.unsupported(&e.key, &e.value, "expected an integer 1-4"),
        }
    }

    // --- Joystick ports ---------------------------------------------
    // Amiberry's device vocabulary (BlitterStudio/amiberry src/cfgfile.cpp
    // `joyportmodes`) is richer than Copperline's five-way [input] port1/2
    // ("mouse"/"joystick"/"cd32"/"analogue"/"none"): the common cases map
    // cleanly, the rest collapse onto the nearest Copperline device and
    // are flagged since the host-input semantics genuinely differ.
    for (uae_key, port) in [("joyport0mode", "port1"), ("joyport1mode", "port2")] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            let (value, note): (&str, Option<&str>) = match e.value.trim() {
                "" => ("none", None),
                "mouse" => ("mouse", None),
                "cd32joy" => ("cd32", None),
                "ajoy" => ("analogue", None),
                "djoy" => ("joystick", None),
                "gamepad" => (
                    "joystick",
                    Some("Amiberry's \"gamepad\" and \"djoy\" (digital joystick) are distinct \
                          host input sources; Copperline only has one \"joystick\" device"),
                ),
                "mousenowheel" => (
                    "mouse",
                    Some("Amiberry's wheel-less mouse variant has no separate Copperline mode"),
                ),
                "cdtvjoy" => (
                    "joystick",
                    Some("CDTV joystick has no dedicated Copperline device; approximated as joystick"),
                ),
                _ => {
                    report.unsupported(&e.key, &e.value, "unrecognized or unsupported port device (e.g. lightpen)");
                    ("", None)
                }
            };
            if !value.is_empty() {
                set_str(&mut doc, &["input"], port, value);
                if let Some(note) = note {
                    annotate(&mut doc, &["input"], port, note);
                }
            }
        }
    }

    // --- Autofire -----------------------------------------------------
    // Amiberry stores an autofire *mode* per port (BlitterStudio/amiberry
    // src/cfgfile.cpp `joyaf`: none/normal/toggle/always/togglebutton),
    // not a rate, and separately for port0/port1; Copperline has exactly
    // one global Hz rate (0 = off), not per-port. Both sources of lossiness
    // -- mode-to-rate and per-port-to-global -- are folded into a single
    // comment rather than silently letting the second key clobber the
    // first with no trace of what happened to it.
    const APPROXIMATED_AUTOFIRE_HZ: i64 = 10;
    let autofire_on = |v: &str| matches!(v, "normal" | "toggle" | "always" | "togglebutton");
    let joyport0af = by_key("joyport0autofire");
    let joyport1af = by_key("joyport1autofire");
    for e in [joyport0af, joyport1af].into_iter().flatten() {
        seen.insert(&e.key, ());
        if !matches!(
            e.value.trim(),
            "none" | "normal" | "toggle" | "always" | "togglebutton"
        ) {
            report.unsupported(&e.key, &e.value, "unrecognized autofire mode");
        }
    }
    match (joyport0af, joyport1af) {
        (None, None) => {}
        _ => {
            let on0 = joyport0af.is_some_and(|e| autofire_on(e.value.trim()));
            let on1 = joyport1af.is_some_and(|e| autofire_on(e.value.trim()));
            let hz = if on0 || on1 {
                APPROXIMATED_AUTOFIRE_HZ
            } else {
                0
            };
            table(&mut doc, &["input"])["autofire_hz"] = toml_edit::value(hz);
            // Off+off is an unambiguous, lossless "off" -- nothing to flag.
            if hz != 0 {
                let source = match (joyport0af, joyport1af) {
                    (Some(a), Some(b)) => {
                        format!("joyport0autofire={}, joyport1autofire={}", a.value, b.value)
                    }
                    (Some(a), None) => format!("joyport0autofire={}", a.value),
                    (None, Some(b)) => format!("joyport1autofire={}", b.value),
                    (None, None) => unreachable!(),
                };
                let collision = if on0 && on1 {
                    " (both ports requested autofire; Copperline's single global rate now \
                       applies to both, and their distinct modes are both lost)"
                } else if joyport0af.is_some() && joyport1af.is_some() {
                    " (only one port requested autofire; Copperline's single global rate now \
                       applies to both)"
                } else {
                    ""
                };
                annotate(
                    &mut doc,
                    &["input"],
                    "autofire_hz",
                    &format!(
                        "from {source} -- Amiberry stores an autofire mode per port, not a rate; \
                         Copperline has one global Hz rate, so this is a guessed default{collision}"
                    ),
                );
            }
        }
    }

    // --- Sound filter -------------------------------------------------
    // Amiberry (BlitterStudio/amiberry src/cfgfile.cpp `soundfiltermode1`):
    // off/emulated/on/fixedonly. Copperline's [audio] audio_filter
    // (src/config/mod.rs parse_audio_filter_mode) only takes auto/on/off.
    if let Some(e) = by_key("sound_filter") {
        seen.insert(&e.key, ());
        match e.value.trim() {
            "off" => set_str(&mut doc, &["audio"], "audio_filter", "off"),
            "on" => set_str(&mut doc, &["audio"], "audio_filter", "on"),
            "emulated" => set_str(&mut doc, &["audio"], "audio_filter", "auto"),
            "fixedonly" => {
                set_str(&mut doc, &["audio"], "audio_filter", "on");
                annotate(
                    &mut doc,
                    &["audio"],
                    "audio_filter",
                    "from sound_filter=fixedonly -- Copperline has no equivalent to Amiberry's \
                     fixed-only filter curve; approximated as always-on",
                );
            }
            _ => report.unsupported(&e.key, &e.value, "unrecognized sound_filter value"),
        }
    }

    // --- known settings with no Copperline equivalent, for visibility ---
    // Not a parsing failure or an oversight -- these are Amiberry/WinUAE
    // concepts Copperline genuinely doesn't have a knob for, called out by
    // name rather than falling into the generic "not recognized" bucket
    // below so a reader can tell "considered and skipped" from "converter
    // doesn't know this key yet".
    for (uae_key, why) in [
        (
            "turbo_emulation",
            "no \"turbo boot\" concept distinct from [emulation] warp_speed",
        ),
        (
            "turbo_boot",
            "no \"turbo boot\" concept distinct from [emulation] warp_speed",
        ),
        (
            "sound_volume",
            "no master output-volume field ([audio] only has floppy_sounds_volume)",
        ),
        (
            "sound_volume_master",
            "no master output-volume field ([audio] only has floppy_sounds_volume)",
        ),
        (
            "cpu_speed",
            "no equivalent: this is a baseline wall-clock throttle (max = bypass pacing \
             entirely, real = pin to authentic 68000 timing), not a clock-rate override \
             ([cpu] clock_mhz is a different thing -- see below); Copperline's [emulation] \
             warp_speed only sets the ceiling of an on-demand turbo toggle that's off by \
             default, so mapping to it would misrepresent \"always run flat out\" as a \
             feature the user has to switch on by hand",
        ),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            report.unsupported(&e.key, &e.value, why);
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

/// WinUAE/Amiberry booleans are spelled `true`/`false`, `yes`/`no`, or
/// `1`/`0` depending on the key's age.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}
