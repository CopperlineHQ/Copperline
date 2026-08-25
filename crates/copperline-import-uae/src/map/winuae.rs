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
    // Several settings have more than one spelling in the wild: WinUAE's
    // own name and the one Amiberry actually writes. Real Amiberry configs
    // carry `rtc=`/`cpu_model=`, not the `cs_rtc=`/`cpu_type=` this mapper
    // was first written against, so both are accepted and the first present
    // wins.
    let by_any = |ks: &[&str]| ks.iter().find_map(|k| by_key(k));

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
    let mut cpu_model = String::new();
    if let Some(e) = by_any(&["cpu_type", "cpu_model"]) {
        seen.insert(&e.key, ());
        // WinUAE spells e.g. "68020", "68020i" (no MMU), "68030mmu",
        // "68040", "68060" -- Copperline only cares about the model digits.
        let digits: String = e.value.chars().take_while(|c| c.is_ascii_digit()).collect();
        let known = ["68000", "68010", "68020", "68030", "68040", "68060"];
        if known.contains(&digits.as_str()) {
            set_str(&mut doc, &["cpu"], "model", &digits);
            cpu_model = digits;
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
    if let Some(e) = by_key("cpu_data_cache") {
        seen.insert(&e.key, ());
        match parse_bool(&e.value) {
            Some(on) => table(&mut doc, &["cpu"])["dcache"] = toml_edit::value(on),
            None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
        }
    }
    // 68060-only: how the emulator handles instructions the real 68060
    // dropped from silicon. Amiberry's raw config key isn't inverted from
    // its name -- confirmed against src/newcpu.cpp: int_no_unimplemented
    // (this key) = true routes those opcodes to the genuine unimplemented-
    // instruction trap (faithful, needs the guest's 68060.library); the
    // GUI's checkbox label inverts the sense for display, the config key
    // doesn't. So true -> "trap", false -> "native", matching Copperline's
    // [cpu] unimplemented exactly.
    if let Some(e) = by_key("cpu_no_unimplemented") {
        seen.insert(&e.key, ());
        // Only the 68060 dropped instructions from silicon, so Copperline
        // rejects `[cpu] unimplemented` on anything else. WinUAE writes the
        // key regardless of the configured CPU, so emitting it unguarded
        // made every non-060 config fail validation.
        if cpu_model == "68060" {
            match parse_bool(&e.value) {
                Some(true) => set_str(&mut doc, &["cpu"], "unimplemented", "trap"),
                Some(false) => set_str(&mut doc, &["cpu"], "unimplemented", "native"),
                None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
            }
        }
    }

    // --- Memory ------------------------------------------------------
    // Each key has its own unit, verified against Amiberry's cfgfile.cpp
    // (`cfgfile_intval`'s trailing argument is a multiplier): chip counts
    // 512K blocks, bogo/slow counts 256K ones, and everything else counts
    // megabytes. They are genuinely different -- a stock A500 writes
    // `chipmem_size=1` (512K) with `bogomem_size=2` (512K, the A501
    // trapdoor), and reading either as megabytes silently inflates the
    // machine.
    for (uae_key, section, unit) in [
        ("chipmem_size", "chip", 512 * 1024),
        ("bogomem_size", "slow", 256 * 1024),
        ("fastmem_size", "fast", 1024 * 1024),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            match uae_mem_bytes(&e.value, unit) {
                // Zero is "none fitted", which is simply the absence of the
                // setting -- emitting `fast = "0M"` would be noise.
                Some(0) => {}
                Some(bytes) => {
                    let size = bytes_to_size(bytes);
                    let size = if section == "chip" {
                        let (clamped, clamp_note) = clamp_chip_mb(&size);
                        if let Some(clamp_note) = clamp_note {
                            annotate(&mut doc, &["memory"], section, &clamp_note);
                        }
                        clamped
                    } else {
                        size
                    };
                    set_str(&mut doc, &["memory"], section, &size);
                }
                None => report.approximated(
                    &e.key,
                    &e.value,
                    format!("couldn't read a {uae_key} value from this"),
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
        ("amiberry.hardfile_path", "harddrives"),
        ("amiberry.cd_path", "cds"),
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
    // only translated when the RTC key says RP5C01 is actually fitted.
    let mut rtc_chip_is_rp5c01 = false;
    if let Some(e) = by_any(&["cs_rtc", "rtc"]) {
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
    if let Some(e) = by_key("scsidevice_disable") {
        seen.insert(&e.key, ());
        match parse_bool(&e.value) {
            Some(on) => {
                table(&mut doc, &["machine"])["rom_scsi_device_disable"] = toml_edit::value(on)
            }
            None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
        }
    }

    // --- Boot straight into the emulation --------------------------------
    // Amiberry's use_gui=no skips its own launcher and boots directly, the
    // same shape as Copperline's [emulation] power_on = true (machine runs
    // immediately rather than sitting powered off on a test screen).
    // use_gui=yes doesn't have a comparable equivalent -- whether
    // Copperline shows its own launcher is a matter of which CLI flags are
    // passed, not a config-file setting -- so that direction is flagged.
    if let Some(e) = by_key("use_gui") {
        seen.insert(&e.key, ());
        match parse_bool(&e.value) {
            Some(false) => table(&mut doc, &["emulation"])["power_on"] = toml_edit::value(true),
            Some(true) => report.unsupported(
                &e.key,
                &e.value,
                "whether Copperline shows its own launcher depends on which CLI flags are \
                 passed, not a config setting; there's nothing to translate this to",
            ),
            None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
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

    // --- RTG board --------------------------------------------------
    // Amiberry (BlitterStudio/amiberry src/gfxboard.cpp `boards[]`,
    // configname field) supports far more RTG chipsets than Copperline
    // models; only Picasso II/II+ and Graffity Z2/Z3 have a Copperline
    // equivalent (src/config/raw.rs RawRtg.card: "z3660"/"picasso2"/
    // "picasso2plus"/"graffityz2"/"graffityz3"/"none") -- everything else
    // (CyberVision, Retina, Piccolo, the built-in UAEGFX ZorroII/III
    // boards, etc.) is flagged unsupported. Amiberry omits the key
    // entirely rather than writing a "none" sentinel when no card is
    // fitted, so absence needs no special handling here.
    let mut card_mapped = false;
    if let Some(e) = by_key("gfxcard_type") {
        seen.insert(&e.key, ());
        let card = match e.value.trim() {
            "PicassoII" => Some("picasso2"),
            "PicassoII+" => Some("picasso2plus"),
            "GraffityZ2" => Some("graffityz2"),
            "GraffityZ3" => Some("graffityz3"),
            _ => None,
        };
        match card {
            Some(card) => {
                set_str(&mut doc, &["rtg"], "card", card);
                card_mapped = true;
            }
            None => report.unsupported(
                &e.key,
                &e.value,
                "Copperline only models Picasso II/II+ and Graffity Z2/Z3; this RTG chipset \
                 has no equivalent",
            ),
        }
    }

    // --- RTG board VRAM -----------------------------------------------
    // Amiberry's gfxcard_size is a plain megabyte count; Copperline's
    // [rtg] vram (Picasso II/II+ and Graffity only) is a closed "1M"/"2M"
    // enum, not a free size, so anything else is flagged rather than
    // guessed at. Setting vram alone doesn't fit a board -- [rtg] card
    // also needs choosing -- so that's called out too, unless gfxcard_type
    // was already translated above, in which case it's redundant.
    if let Some(e) = by_key("gfxcard_size") {
        seen.insert(&e.key, ());
        let mapped = match e.value.trim() {
            "1" => Some("1M"),
            "2" => Some("2M"),
            _ => None,
        };
        match mapped {
            Some(vram) => {
                set_str(&mut doc, &["rtg"], "vram", vram);
                if !card_mapped {
                    annotate(
                        &mut doc,
                        &["rtg"],
                        "vram",
                        "from gfxcard_size -- also set [rtg] card (e.g. \"graffityz3\") for \
                         this to take effect; the source config's board type wasn't translated",
                    );
                }
            }
            None => report.unsupported(
                &e.key,
                &e.value,
                "Copperline's [rtg] vram only takes \"1M\" or \"2M\" (Picasso II/II+ and \
                 Graffity); other sizes have no equivalent",
            ),
        }
    }

    // --- Identification board -------------------------------------------
    // uae_hide_autoconfig hides UAE's own identification autoconfig
    // device; the equivalent Copperline knob is a top-level key, not
    // under any section (src/config/raw.rs RawConfig.identify: "false
    // drops the Copperline identification board from the autoconfig
    // chain").
    if let Some(e) = by_key("uae_hide_autoconfig") {
        seen.insert(&e.key, ());
        match parse_bool(&e.value) {
            Some(hide) => doc["identify"] = toml_edit::value(!hide),
            None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
        }
    }

    // --- Serial port ---------------------------------------------------
    // Amiberry's serial_port is a free-form target string; only the
    // TCP://host:port form has a clean Copperline equivalent ([serial]
    // mode = "tcp", listen = the host:port). Real hardware device paths
    // (e.g. "/dev/ttyUSB0", "COM1") and other schemes have no translation
    // and are left to the generic unrecognized-key fallback below.
    if let Some(e) = by_key("serial_port") {
        let value = e.value.trim();
        if let Some(addr) = value
            .strip_prefix("TCP://")
            .or_else(|| value.strip_prefix("tcp://"))
        {
            seen.insert(&e.key, ());
            set_str(&mut doc, &["serial"], "mode", "tcp");
            set_str(&mut doc, &["serial"], "listen", addr);
        }
    }

    // --- SCSI host adapter -----------------------------------------------
    // Same shape as the lide keys below: Amiberry has a separate ROM-file
    // key per controller rather than Copperline's single [scsi] controller
    // = "..." selector, and only one adapter can be fitted at once, so more
    // than one of these present at once is a real conflict to flag rather
    // than letting the last one silently win. a2091/a4091 carry a real ROM
    // path; a3000 (the built-in A3000 SDMAC) uses the same ":ENABLED"
    // sentinel convention as Toccata -- Copperline's [scsi] has no ROM
    // field for it (the A3000's boot code lives in the machine ROM, not a
    // separate image), so a real path there is flagged instead of dropped
    // silently.
    let scsi_adapters: Vec<(&Entry, &str)> = [
        ("a2091_rom_file", "a2091"),
        ("a4091_rom_file", "a4091"),
        ("scsi_a3000_rom_file", "a3000"),
    ]
    .into_iter()
    .filter_map(|(key, controller)| by_key(key).map(|e| (e, controller)))
    .collect();
    match scsi_adapters.as_slice() {
        [] => {}
        [(e, controller)] => {
            seen.insert(&e.key, ());
            set_str(&mut doc, &["scsi"], "controller", controller);
            if *controller == "a3000" {
                // The A3000 SDMAC is the motherboard's own controller, not a
                // fittable Zorro board, so Copperline requires [machine]
                // profile = "A3000" for controller = "a3000" to validate.
                // Its presence in the source config is unambiguous enough
                // to set the profile automatically rather than just flag it
                // -- this key doesn't exist unless the machine really is an
                // A3000.
                set_str(&mut doc, &["machine"], "profile", "A3000");
                annotate(
                    &mut doc,
                    &["machine"],
                    "profile",
                    "inferred from scsi_a3000_rom_file: that ROM key only makes sense on an \
                     A3000 (the motherboard SDMAC), so the profile was set to match",
                );
                if !e.value.trim().eq_ignore_ascii_case(":ENABLED") {
                    report.approximated(
                        &e.key,
                        &e.value,
                        "Copperline's [scsi] has no ROM field for the built-in A3000 SDMAC \
                         (its boot code lives in the machine ROM); only \"controller\" was set",
                    );
                }
            } else {
                set_str(&mut doc, &["scsi"], "rom", &e.value);
            }
        }
        _ => {
            let keys: Vec<&str> = scsi_adapters.iter().map(|(e, _)| e.key.as_str()).collect();
            for (e, _) in &scsi_adapters {
                seen.insert(&e.key, ());
                report.unsupported(
                    &e.key,
                    &e.value,
                    format!(
                        "{} are all set, but Copperline's [scsi] controller can only be one \
                         adapter at a time; pick one by hand",
                        keys.join(", ")
                    ),
                );
            }
        }
    }

    // --- lide.device-compatible IDE board --------------------------------
    // Amiberry has a separate ROM-file key per board personality rather
    // than Copperline's single [lide] board = "..." selector; each key
    // implies both the ROM path and which personality is fitted. Only one
    // board can be fitted at once, so if a source config somehow has both
    // (hand-edited, or two emulator versions' settings merged), that's a
    // real conflict worth flagging rather than letting the second key
    // silently win.
    let alfapower = by_key("alfapower_rom_file");
    let ripple = by_key("ripple_rom_file");
    match (alfapower, ripple) {
        (Some(a), Some(r)) => {
            seen.insert(&a.key, ());
            seen.insert(&r.key, ());
            report.unsupported(
                &a.key,
                &a.value,
                "both alfapower_rom_file and ripple_rom_file are set, but Copperline's \
                 [lide] board can only be one personality at a time; pick one by hand",
            );
            report.unsupported(
                &r.key,
                &r.value,
                "both alfapower_rom_file and ripple_rom_file are set, but Copperline's \
                 [lide] board can only be one personality at a time; pick one by hand",
            );
        }
        (Some(e), None) | (None, Some(e)) => {
            seen.insert(&e.key, ());
            let board = if e.key == "alfapower_rom_file" {
                "atbus2008"
            } else {
                "ripple"
            };
            set_str(&mut doc, &["lide"], "board", board);
            set_str(&mut doc, &["lide"], "rom", &e.value);
        }
        (None, None) => {}
    }

    // --- Toccata sound board --------------------------------------------
    // Amiberry's toccata_rom_file doubles as the board's fit switch: a
    // literal ":ENABLED" sentinel means the board is fitted with no ROM
    // file selected. Copperline's [toccata] only has an enabled flag (no
    // ROM file of its own), so any non-empty value here means "fitted" --
    // a real path is flagged since the path itself has nowhere to go.
    if let Some(e) = by_key("toccata_rom_file") {
        seen.insert(&e.key, ());
        let value = e.value.trim();
        if !value.is_empty() {
            table(&mut doc, &["toccata"])["enabled"] = toml_edit::value(true);
            if !value.eq_ignore_ascii_case(":ENABLED") {
                annotate(
                    &mut doc,
                    &["toccata"],
                    "enabled",
                    &format!(
                        "from toccata_rom_file={value} -- Copperline's Toccata emulation \
                         doesn't take a ROM file, only fitted/not fitted; the path itself \
                         wasn't translated"
                    ),
                );
            }
        }
    }

    // --- Machine model --------------------------------------------------
    // `chipset_compatible` is the machine whose chipset quirks WinUAE is
    // imitating ("A500", "A1200", ...), which is the closest thing the
    // format has to naming a model. Mapped to `[machine] profile` so the
    // result inherits Copperline's own per-model wiring (Gayle, the RTC
    // socket, and the rest) rather than defaulting to a bare machine; the
    // explicit [cpu]/[chipset]/[memory] keys emitted above still override
    // whatever the profile would have supplied.
    if let Some(e) = by_key("chipset_compatible") {
        seen.insert(&e.key, ());
        let profile = match e.value.trim().to_ascii_uppercase().as_str() {
            "A1000" => Some("A1000"),
            "A500" => Some("A500"),
            "A500+" => Some("A500Plus"),
            "A600" => Some("A600"),
            "A1200" => Some("A1200"),
            "A3000" => Some("A3000"),
            "A4000" => Some("A4000"),
            "CDTV" => Some("CDTV"),
            "CD32" => Some("CD32"),
            _ => None,
        };
        match profile {
            Some(profile) => set_str(&mut doc, &["machine"], "profile", profile),
            None => report.approximated(
                &e.key,
                &e.value,
                "no matching Copperline machine profile; the machine is built from the \
                 explicit CPU/chipset/memory keys alone",
            ),
        }
    }

    // --- Memory (the expansions beyond chip/fast/slow) -------------------
    for (uae_key, section, what) in [
        ("z3mem_size", "z3", "Zorro III"),
        ("mbresmem_size", "motherboard", "motherboard"),
    ] {
        if let Some(e) = by_key(uae_key) {
            seen.insert(&e.key, ());
            match e.value.trim().parse::<u64>() {
                // 0 is "none fitted", which is the absence of the key.
                Ok(0) => {}
                Ok(mb) => set_str(&mut doc, &["memory"], section, &format!("{mb}M")),
                Err(_) => report.unsupported(
                    &e.key,
                    &e.value,
                    format!("expected a {what} RAM size in MB"),
                ),
            }
        }
    }

    // --- Audio ------------------------------------------------------
    if let Some(e) = by_key("sound_channels") {
        seen.insert(&e.key, ());
        match e.value.trim().to_ascii_lowercase().as_str() {
            "stereo" => set_str(&mut doc, &["audio"], "channel_mode", "stereo"),
            "mono" => set_str(&mut doc, &["audio"], "channel_mode", "mono"),
            _ => report.unsupported(
                &e.key,
                &e.value,
                "Copperline's [audio] channel_mode is only \"stereo\" or \"mono\"",
            ),
        }
    }

    // --- CD image -----------------------------------------------------
    // `cdimage0=/path/to.iso,disabled` -- the trailing flag is the drive's
    // own enabled state, not part of the path.
    if let Some(e) = by_key("cdimage0") {
        seen.insert(&e.key, ());
        let value = e.value.trim();
        let (path, flag) = match value.rsplit_once(',') {
            Some((path, flag)) => (path, Some(flag.trim())),
            None => (value, None),
        };
        if path.is_empty() {
            // An empty entry is just "no disc", not something to report.
        } else if flag == Some("disabled") {
            report.approximated(
                &e.key,
                &e.value,
                "the CD drive was disabled in the source config, so the image is left out; \
                 set [cd] image by hand to insert it",
            );
        } else {
            set_str(&mut doc, &["cd"], "image", path);
        }
    }

    // --- Host directory mounts ------------------------------------------
    // `filesystem2=rw,DH0:Workbench:/host/path,0`: access, then
    // device:volume:path, then boot priority. Copperline's [[filesys]] is
    // the same idea -- a host directory handed to the guest as a volume --
    // so these map across directly. Amiberry writes a paired
    // `uaehfN=dir,<the same fields>` for every one of these; those are
    // consumed here too rather than mapped again, or every mount would be
    // emitted twice.
    let mut filesys: Vec<FilesysMount> = Vec::new();
    for e in entries.iter().filter(|e| e.key == "filesystem2") {
        seen.insert(&e.key, ());
        match parse_filesystem2(&e.value) {
            Some(mount) => filesys.push(mount),
            None => report.unsupported(
                &e.key,
                &e.value,
                "could not read this as access,DEVICE:Volume:/path,bootpri",
            ),
        }
    }
    for e in entries
        .iter()
        .filter(|e| e.key.starts_with("uaehf") && e.key[5..].chars().all(|c| c.is_ascii_digit()))
    {
        let rest = e.value.trim().strip_prefix("dir,");
        match rest {
            // The directory form duplicates a `filesystem2` line, already
            // taken above.
            Some(rest)
                if filesys
                    .iter()
                    .any(|m| parse_filesystem2(rest).as_ref() == Some(m)) =>
            {
                seen.insert(&e.key, ());
            }
            // A `hdf,...` entry is a real hardfile image, which needs a
            // controller decision Copperline cannot infer -- left for the
            // generic bucket below to report by name.
            _ => {}
        }
    }
    if !filesys.is_empty() {
        let mut array = toml_edit::ArrayOfTables::new();
        for mount in &filesys {
            let mut t = toml_edit::Table::new();
            t["path"] = toml_edit::value(mount.path.as_str());
            if !mount.volume.is_empty() {
                t["volume"] = toml_edit::value(mount.volume.as_str());
            }
            if mount.bootpri != -128 {
                t["bootpri"] = toml_edit::value(i64::from(mount.bootpri));
            }
            if mount.readonly {
                t["readonly"] = toml_edit::value(true);
            }
            array.push(t);
        }
        doc["filesys"] = toml_edit::Item::ArrayOfTables(array);
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
        (
            "uaeserial",
            "selects UAE's own internal custom serial.device implementation, not a host \
             wiring choice; there's nothing in [serial] this corresponds to",
        ),
        (
            "amiberry.expansion_gui_page",
            "just remembers which tab of Amiberry's own RTG config page was last open; not \
             a machine setting",
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

/// A WinUAE/Amiberry memory-size integer in bytes. `unit` is the key's own
/// multiplier (see the call site). `chipmem_size` alone has two sentinel
/// values below one block, which Amiberry special-cases the same way:
/// `-1` is 128K and `0` is 256K, rather than "none".
fn uae_mem_bytes(value: &str, unit: u64) -> Option<u64> {
    let n: i64 = value.trim().parse().ok()?;
    if unit == 512 * 1024 {
        return Some(match n {
            -1 => 128 * 1024,
            0 => 256 * 1024,
            n if n > 0 => (n as u64) * unit,
            _ => return None,
        });
    }
    if n < 0 {
        return None;
    }
    Some((n as u64) * unit)
}

/// A byte count as Copperline spells memory sizes: whole megabytes as `M`,
/// anything smaller as `K`.
fn bytes_to_size(bytes: u64) -> String {
    if bytes.is_multiple_of(1024 * 1024) {
        format!("{}M", bytes / (1024 * 1024))
    } else {
        format!("{}K", bytes / 1024)
    }
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

/// One `[[filesys]]` mount read out of a `filesystem2`/`uaehf` line.
#[derive(Debug, PartialEq, Eq)]
struct FilesysMount {
    path: String,
    volume: String,
    bootpri: i16,
    readonly: bool,
}

/// Parse `rw,DH0:Workbench:/host/path,0` (the body of a `filesystem2`, or a
/// `uaehf` line with its leading `dir,` already stripped). The boot priority
/// is peeled off the end and the access flag off the front, so a path
/// holding a comma only breaks the cases a path holding a comma would break
/// anyway; the device/volume/path triple is split on the first two colons,
/// leaving any colon inside the path alone.
fn parse_filesystem2(value: &str) -> Option<FilesysMount> {
    let value = value.trim();
    let (head, bootpri) = value.rsplit_once(',')?;
    let (access, spec) = head.split_once(',')?;
    let bootpri: i16 = bootpri.trim().parse().ok()?;
    let mut parts = spec.splitn(3, ':');
    let _device = parts.next()?;
    let volume = parts.next()?;
    let path = parts.next()?;
    if path.is_empty() {
        return None;
    }
    Some(FilesysMount {
        path: path.to_string(),
        volume: volume.to_string(),
        bootpri,
        readonly: access.trim().eq_ignore_ascii_case("ro"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(text: &str) -> String {
        let entries = crate::parse::parse(text);
        map(&entries).doc.to_string()
    }

    #[test]
    fn memory_keys_each_use_their_own_unit() {
        // Verified against Amiberry's cfgfile.cpp: chip counts 512K
        // blocks, bogo 256K ones, fast megabytes. A stock A500 writes
        // exactly this, and reading either of the first two as megabytes
        // would silently inflate the machine.
        let out = convert("chipmem_size=1\nbogomem_size=2\nfastmem_size=8\n");
        assert!(out.contains(r#"chip = "512K""#), "{out}");
        assert!(out.contains(r#"slow = "512K""#), "{out}");
        assert!(out.contains(r#"fast = "8M""#), "{out}");

        // A stock A1200's 2MB of chip RAM, which must not trip the 2M clamp.
        let out = convert("chipmem_size=4\n");
        assert!(out.contains(r#"chip = "2M""#), "{out}");
        assert!(
            !out.contains("clamped"),
            "2M is the ceiling, not over it: {out}"
        );
    }

    #[test]
    fn chipmem_sentinels_below_one_block_are_honoured() {
        // Amiberry special-cases these two rather than treating them as
        // "n blocks": -1 is 128K and 0 is 256K, neither of them "none".
        assert!(convert("chipmem_size=-1\n").contains(r#"chip = "128K""#));
        assert!(convert("chipmem_size=0\n").contains(r#"chip = "256K""#));
    }

    #[test]
    fn zero_sized_expansions_are_left_out_entirely() {
        // "none fitted" is the absence of the key, not `fast = "0M"`.
        let out = convert("fastmem_size=0\nbogomem_size=0\nz3mem_size=0\nmbresmem_size=0\n");
        assert!(!out.contains("fast ="), "{out}");
        assert!(!out.contains("slow ="), "{out}");
        assert!(!out.contains("z3 ="), "{out}");
        assert!(!out.contains("motherboard ="), "{out}");
    }

    #[test]
    fn the_amiberry_spellings_of_rtc_and_cpu_are_accepted() {
        // Real Amiberry configs write `rtc=`/`cpu_model=`; the WinUAE
        // spellings this mapper was first written against never appear, so
        // both had to be accepted or neither setting ever imported.
        let out = convert("rtc=MSM6242B\ncpu_model=68020\n");
        assert!(out.contains(r#"rtc_chip = "MSM6242""#), "{out}");
        assert!(out.contains("rtc = true"), "{out}");
        assert!(out.contains(r#"model = "68020""#), "{out}");
    }

    #[test]
    fn chipset_compatible_picks_the_machine_profile() {
        assert!(convert("chipset_compatible=A1200\n").contains(r#"profile = "A1200""#));
        assert!(convert("chipset_compatible=A500\n").contains(r#"profile = "A500""#));
    }

    #[test]
    fn filesystem2_becomes_a_filesys_mount() {
        let out = convert(
            "filesystem2=rw,DH0:Workbench:/host/wb,0\n\
             filesystem2=ro,DH1:Transfer:/host/xfer,-128\n",
        );
        assert!(out.contains(r#"path = "/host/wb""#), "{out}");
        assert!(out.contains(r#"volume = "Workbench""#), "{out}");
        assert!(out.contains(r#"volume = "Transfer""#), "{out}");
        assert!(out.contains("readonly = true"), "the ro mount: {out}");
        // -128 is the default, so it is left implicit; 0 is not.
        assert!(out.contains("bootpri = 0"), "{out}");
    }

    #[test]
    fn a_uaehf_directory_entry_does_not_duplicate_its_filesystem2_twin() {
        // Amiberry writes both lines for every directory mount; importing
        // each would give the guest the same volume twice.
        let out = convert(
            "filesystem2=rw,DH0:Workbench:/host/wb,0\n\
             uaehf0=dir,rw,DH0:Workbench:/host/wb,0\n",
        );
        assert_eq!(out.matches(r#"path = "/host/wb""#).count(), 1, "{out}");
    }

    #[test]
    fn a_disabled_cd_image_is_flagged_rather_than_inserted() {
        let entries = crate::parse::parse("cdimage0=/discs/os32.iso,disabled\n");
        let out = map(&entries);
        assert!(!out.doc.to_string().contains("image ="), "{}", out.doc);
        assert_eq!(out.report.flagged.len(), 1);

        let out = convert("cdimage0=/discs/os32.iso\n");
        assert!(out.contains(r#"image = "/discs/os32.iso""#), "{out}");
    }
}
