//! FS-UAE `Config.fs-uae` -> Copperline TOML. A different vocabulary from
//! WinUAE/Amiberry, not a rename: sizes are in KB rather than an
//! index/MB mix, and `amiga_model` is a database preset that implies a
//! chipset+CPU+RAM combination rather than each axis being spelled out
//! separately. An explicit `chipset`/`cpu`/`*_memory` always overrides the
//! preset, matching FS-UAE's own precedence.

use super::{
    addressing_note, annotate, apply_drive_sounds, can_fit_fpu, clamp_chip_mb, cpu_part,
    leading_number, parse_bool, parse_cpu_type, set_cpu_fpu, set_str, table, DriveSound,
    MapOutcome, CPU_CORES,
};
use crate::parse::Entry;
use crate::report::ImportReport;
use std::collections::HashMap;
use toml_edit::DocumentMut;

/// An `amiga_model` preset as FS-UAE 3.1.66 builds it: UAE's built-in
/// quickstart for the model (the `bip_*` functions in FS-UAE's bundled
/// src/cfgfile.cpp, run at FS-UAE's accuracy 1), then the CPU defaults that
/// src/fs-uae/config-model.c gives the model and the accelerator board it
/// fits (config-accelerator.c). Not exhaustive: other presets (CDTV, A1000,
/// the PowerPC boards, ...) are flagged rather than guessed at.
#[derive(Clone, Copy)]
struct Preset {
    chipset: &'static str,
    profile: &'static str,
    chip: &'static str,
    /// The CPU with no `cpu` option, as a `cpu` value (see [`fsuae_cpu`]).
    cpu: &'static str,
    /// Whether an FPU is fitted with no `cpu` option (`default_fpu`), and
    /// beside a 68EC020/68030 that a `cpu` option names
    /// (`default_fpu_noninternal`).
    fpu: bool,
    fpu_external: bool,
    /// Ramsey motherboard RAM (the quickstart's `mbresmem_low_size`).
    motherboard: Option<&'static str>,
    /// The accelerator board fitted when no `accelerator` option names one.
    accelerator: Option<&'static str>,
    /// The CD32 Full Motion Video module.
    fmv: bool,
}

fn model_preset(model: &str) -> Option<Preset> {
    const A500: Preset = Preset {
        chipset: "OCS",
        profile: "A500",
        chip: "512K",
        cpu: "68000",
        fpu: false,
        fpu_external: false,
        motherboard: None,
        accelerator: None,
        fmv: false,
    };
    // A plain A1200 runs its 68020 on 24 address lines: the 68EC020.
    const A1200: Preset = Preset {
        chipset: "AGA",
        profile: "A1200",
        chip: "2M",
        cpu: "68ec020",
        ..A500
    };
    // The A3000 and A4000 quickstarts fit a 68882 and 8M of motherboard
    // RAM. FS-UAE runs the A4000's 68030 with MMU emulation off and calls it
    // the 68EC030, as the A4000/030 shipped.
    const A4000: Preset = Preset {
        chipset: "AGA",
        profile: "A4000",
        chip: "2M",
        cpu: "68ec030",
        fpu: true,
        fpu_external: true,
        motherboard: Some("8M"),
        ..A500
    };
    const CD32: Preset = Preset {
        chipset: "AGA",
        profile: "CD32",
        chip: "2M",
        cpu: "68ec020",
        ..A500
    };
    Some(match model.to_ascii_uppercase().as_str() {
        "A500" => A500,
        "A500+" | "A500PLUS" => Preset {
            chipset: "ECS",
            profile: "A500Plus",
            chip: "1M",
            ..A500
        },
        "A600" => Preset {
            chipset: "ECS",
            profile: "A600",
            chip: "1M",
            ..A500
        },
        "A1200" | "A1200/3.0" => A1200,
        "A1200/020" => Preset {
            cpu: "68020",
            ..A1200
        },
        // The Blizzard presets turn 32-bit addressing on, so `cpu = auto`
        // leaves them a full 68020 rather than the board's CPU.
        "A1200/1230" => Preset {
            cpu: "68020",
            accelerator: Some("blizzard-1230-iv"),
            ..A1200
        },
        "A1200/1240" => Preset {
            cpu: "68020",
            accelerator: Some("blizzard-1240"),
            ..A1200
        },
        "A1200/1260" => Preset {
            cpu: "68020",
            accelerator: Some("blizzard-1260"),
            ..A1200
        },
        // Copperline has no A2000 profile; the A500 is the same chipset
        // generation and the closest machine it does model.
        "A2000" => Preset {
            chipset: "ECS",
            chip: "1M",
            ..A500
        },
        "A3000" => Preset {
            chipset: "ECS",
            profile: "A3000",
            cpu: "68030",
            ..A4000
        },
        "A4000" | "A4000/030" => A4000,
        "A4000/040" => Preset {
            cpu: "68040",
            fpu_external: false,
            ..A4000
        },
        "CD32" => CD32,
        "CD32/FMV" => Preset { fmv: true, ..CD32 },
        _ => return None,
    })
}

/// An `accelerator` board as config-accelerator.c fits it: (name, its CPU
/// as a `cpu` value, board RAM in megabytes). FS-UAE runs the 1240's and
/// 1260's CPUs with MMU emulation off ("68040-NOMMU"/"68060-NOMMU"), but
/// the boards carry the full parts, so they import as those.
fn accelerator_board(value: &str) -> Option<(&'static str, &'static str, u64)> {
    Some(match fs_value(value).as_str() {
        "blizzard1230iv" => ("Blizzard 1230 IV", "68ec030", 32),
        "blizzard1240" => ("Blizzard 1240", "68040", 32),
        "blizzard1260" => ("Blizzard 1260", "68060", 32),
        _ => return None,
    })
}

/// Whether Ramsey takes `mb` megabytes of motherboard RAM, as Copperline's
/// `validate_mb_ram` checks it: 1M-4M on 1M banks or 8M, 12M or 16M on 4M
/// banks, and on the A4000's Ramsey-07 also 20M-64M in 4M steps.
fn ramsey_takes(mb: u64, a4000: bool) -> bool {
    mb <= 4 || (mb.is_multiple_of(4) && mb <= if a4000 { 64 } else { 16 })
}

/// A value the way FS-UAE compares them (`fs_uae_values_matches` in
/// src/fs-uae/config-common.c): case-insensitively, ignoring everything but
/// letters, digits and '+', so "68040-NOMMU" is "68040nommu".
fn fs_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The CPU as FS-UAE leaves it in UAE's prefs: the core, 24-bit
/// addressing, whether an FPU is fitted, and whether the part is an EC one
/// without its MMU. FS-UAE runs its EC parts with MMU emulation off, but so
/// it does its -NOMMU parts and anything a `uae_*` line sets up, and
/// WinUAE's MMU "None" is the same speed setting on a full part; so the EC
/// parts are tracked by name, and only a `cpu` value makes one.
#[derive(Clone, Copy)]
struct CpuState {
    core: u32,
    narrow: bool,
    ec: bool,
    fpu: bool,
}

impl CpuState {
    fn model(self) -> &'static str {
        cpu_part(self.core, self.narrow, !self.ec, self.fpu)
    }
}

/// The machine's CPU before its `cpu` option: `base` is the preset's own
/// (FS-UAE's A500 when no model is named; unknown for a preset this
/// converter does not recognize), which FS-UAE keeps for `cpu = auto` or a
/// value it does not know, and `board` an accelerator board's, which it
/// takes instead when there is no `cpu` option at all (`configure_cpu`).
/// `fpu_external` is the 6888x the machine fits beside a 68020/68030 that a
/// `cpu` option names (see [`Preset`]).
#[derive(Clone, Copy)]
struct MachineCpu {
    base: Option<CpuState>,
    board: Option<CpuState>,
    fpu_external: bool,
}

/// What a `cpu` value does to the FPU.
#[derive(Clone, Copy)]
enum PartFpu {
    /// The part settles it.
    Fixed(bool),
    /// The 6888x the machine fits beside an external-FPU CPU
    /// (`default_fpu_noninternal`).
    External,
}

/// A `cpu` value (already through [`fs_value`]) as `configure_cpu` sets it
/// up: (core, 24-bit addressing, an EC part, FPU).
fn fsuae_cpu(value: &str) -> Option<(u32, bool, bool, PartFpu)> {
    use PartFpu::{External, Fixed};
    Some(match value {
        "68000" => (68000, true, false, Fixed(false)),
        "68010" => (68010, true, false, Fixed(false)),
        "68ec020" => (68020, true, false, External),
        "68020" => (68020, false, false, External),
        "68ec030" => (68030, false, true, Fixed(false)),
        "68030" => (68030, false, false, External),
        "68ec040" => (68040, false, true, Fixed(false)),
        "68lc040" => (68040, false, false, Fixed(false)),
        "68040nommu" | "68040" => (68040, false, false, Fixed(true)),
        "68ec060" => (68060, false, true, Fixed(false)),
        "68lc060" => (68060, false, false, Fixed(false)),
        "68060nommu" | "68060" => (68060, false, false, Fixed(true)),
        _ => return None,
    })
}

/// The CPU a `cpu` value makes, on a machine that fits `fpu_external`
/// beside a 68020/68030.
fn named_cpu(part: (u32, bool, bool, PartFpu), fpu_external: bool) -> CpuState {
    let (core, narrow, ec, part_fpu) = part;
    let fpu = match part_fpu {
        PartFpu::Fixed(fpu) => fpu,
        PartFpu::External => fpu_external,
    };
    CpuState {
        core,
        narrow,
        ec,
        fpu,
    }
}

/// The machine's CPUs before its `cpu` option (see [`MachineCpu`]). A
/// preset this converter does not recognize is taken to fit no 6888x of its
/// own, as most models do not.
fn machine_cpu(preset: Option<Preset>, board_cpu: Option<&str>) -> MachineCpu {
    let fpu_external = preset.is_some_and(|p| p.fpu_external);
    // With no `cpu` option, FS-UAE keeps the model's default FPU.
    let base = preset.and_then(|p| {
        let (core, narrow, ec, _) = fsuae_cpu(p.cpu)?;
        Some(CpuState {
            core,
            narrow,
            ec,
            fpu: p.fpu,
        })
    });
    // The board's CPU is taken as if a `cpu` option named it.
    let board = board_cpu
        .and_then(fsuae_cpu)
        .map(|part| named_cpu(part, fpu_external));
    MachineCpu {
        base,
        board,
        fpu_external,
    }
}

fn mmu_off_note(model: &str) -> String {
    format!(
        "FS-UAE switches MMU emulation off here; Copperline's {model} keeps its MMU, which \
         stays idle unless software enables translation (the 68EC parts have none)"
    )
}

/// Fit or remove the FPU, saying so when an on-chip FPU on a 68EC040/68EC060
/// makes it the full part: no real chip has one without the MMU, so this is
/// FS-UAE's -NOMMU CPU.
fn set_fpu(cpu: &mut CpuState, on: bool, e: &Entry, report: &mut ImportReport) {
    if on && !cpu.fpu && cpu.ec && cpu.core >= 68040 {
        let full = cpu_part(cpu.core, false, true, true);
        report.approximated(&e.key, &e.value, mmu_off_note(full));
    }
    cpu.fpu = on;
}

/// Apply an `mmu` or `uae_mmu_model` line: MMU emulation for the core, or
/// off. Off is FS-UAE's speed setting rather than a different chip, so it
/// leaves the part alone and says so; the EC parts are `cpu` values of
/// their own.
fn set_mmu(cpu: &mut CpuState, e: &Entry, report: &mut ImportReport) {
    let value = e.value.trim().to_ascii_lowercase();
    if value.contains("ec") {
        // `configure_cpu` knows only the numbers, and FS-UAE's UAE core
        // reads mmu_model with `_tstol`: it has no EC MMU.
        report.unsupported(
            &e.key,
            &e.value,
            "FS-UAE has no EC MMU setting (its UAE core reads the MMU as a number); name the \
             part with cpu = 68EC030, 68EC040 or 68EC060",
        );
        return;
    }
    match leading_number(&value) {
        Some(0) => {
            if !cpu.ec && cpu.core >= 68030 {
                report.approximated(&e.key, &e.value, mmu_off_note(cpu.model()));
            }
        }
        // An MMU added to an EC part makes the full part (an FPU-less
        // 68EC040/68EC060 the LC one).
        Some(n) if n == cpu.core && n >= 68030 => cpu.ec = false,
        Some(n) if CPU_CORES.contains(&n) => report.unsupported(
            &e.key,
            &e.value,
            format!(
                "FS-UAE emulates an MMU only for a 68030, 68040 or 68060, each with its own CPU; \
                 this one does not fit a {}",
                cpu.model()
            ),
        ),
        _ => report.unsupported(
            &e.key,
            &e.value,
            "unrecognized MMU; expected 0, 68030, 68040 or 68060",
        ),
    }
}

/// FS-UAE's raw UAE CPU options, handed to UAE's own parser.
const UAE_CPU_KEYS: [&str; 5] = [
    "uae_cpu_type",
    "uae_cpu_model",
    "uae_fpu_model",
    "uae_mmu_model",
    "uae_cpu_24bit_addressing",
];

/// Map FS-UAE's `cpu`, `fpu` and `mmu` options, then its raw `uae_*` CPU
/// options, onto `[cpu] model` and `fpu`, returning the model (`None` when
/// the machine's CPU is unknown). Verified against FS-UAE 3.1.66:
/// doc/options/cpu lists the parts, and `configure_cpu` in
/// src/fs-uae/config-hardware.c turns each into UAE's cpu_model, fpu_model,
/// mmu_model and cpu_24bit_addressing, then applies `fpu` (0, 68881, 68882,
/// 68040, 68060) and the undocumented `mmu` (0, 68030, 68040, 68060) on
/// top, warning about and ignoring any that do not fit the core; "auto", or
/// no option, keeps the machine's default. The EC parts are named directly,
/// and the 68LC040/68LC060 are the 68040/68060 without their FPU.
/// "68040-NOMMU"/"68060-NOMMU" are the full parts with MMU emulation
/// switched off for speed -- UAE's mmu_model=0, WinUAE's MMU "None" -- and
/// no real 68040/68060 has its FPU without the MMU, so they become the full
/// Copperline parts, as `mmu = 0` leaves its part alone.
///
/// Only after all of that does FS-UAE hand each `uae_*` line to UAE's own
/// parser (main.c calls `fs_uae_read_custom_uae_options` after
/// `fs_uae_configure_amiga_hardware`), in file order. That order matters:
/// UAE clears the FPU with every `cpu_model` line, and its core in FS-UAE
/// reads `mmu_model` as a number, with no EC MMU.
fn map_cpu<'a>(
    entries: &'a [Entry],
    seen: &mut HashMap<&'a str, ()>,
    doc: &mut DocumentMut,
    report: &mut ImportReport,
    machine: MachineCpu,
) -> Option<&'static str> {
    const NO_MACHINE_CPU: &str = "the CPU comes from an amiga_model preset this converter does \
                                  not recognize; set [cpu] model and fpu by hand";
    let by_key = |k: &str| entries.iter().rev().find(|e| e.key == k);
    let is_uae_cpu_key = |e: &&Entry| UAE_CPU_KEYS.contains(&e.key.as_str());
    for e in entries
        .iter()
        .filter(|e| matches!(e.key.as_str(), "cpu" | "fpu" | "mmu") || is_uae_cpu_key(e))
    {
        seen.insert(&e.key, ());
    }

    // Any `cpu` option, "auto" and unknown values included, passes over the
    // board's CPU.
    let cpu_entry = by_key("cpu");
    let mut state = match cpu_entry {
        Some(_) => machine.base,
        None => machine.board.or(machine.base),
    };
    let mut cpu_named = false;
    if let Some(e) = cpu_entry {
        let value = fs_value(&e.value);
        if value != "auto" {
            match fsuae_cpu(&value) {
                Some(part) => {
                    let cpu = named_cpu(part, machine.fpu_external);
                    if value.ends_with("nommu") {
                        report.approximated(&e.key, &e.value, mmu_off_note(cpu.model()));
                    }
                    state = Some(cpu);
                    cpu_named = true;
                }
                // FS-UAE warns and keeps the machine's CPU.
                None => report.unsupported(
                    &e.key,
                    &e.value,
                    "unrecognized CPU; FS-UAE knows 68000, 68010, 68EC020, 68020, 68EC030, \
                     68030, 68EC040, 68LC040, 68040-NOMMU, 68040, 68EC060, 68LC060, \
                     68060-NOMMU and 68060",
                ),
            }
        }
    }

    // The last line that named an FPU, and the last that set the core or
    // the address width.
    let mut fpu_entry: Option<&Entry> = None;
    let mut width_entry: Option<&Entry> = None;
    if let Some(e) = by_key("fpu").filter(|e| fs_value(&e.value) != "auto") {
        match state.as_mut() {
            None => report.unsupported(&e.key, &e.value, NO_MACHINE_CPU),
            Some(cpu) => {
                let value = fs_value(&e.value);
                let fits = match value.as_str() {
                    "0" => Some(true),
                    "68881" | "68882" => Some(matches!(cpu.core, 68020 | 68030)),
                    "68040" => Some(cpu.core == 68040),
                    "68060" => Some(cpu.core == 68060),
                    _ => None,
                };
                match fits {
                    Some(true) => {
                        set_fpu(cpu, value != "0", e, report);
                        fpu_entry = Some(e);
                    }
                    Some(false) => report.unsupported(
                        &e.key,
                        &e.value,
                        format!(
                            "FS-UAE ignores this FPU on a {}: a 68881/68882 pairs only with a \
                             68020/68030, the 68040/68060 FPU only with its own CPU",
                            cpu.model()
                        ),
                    ),
                    None => report.unsupported(
                        &e.key,
                        &e.value,
                        "unrecognized FPU; expected 0, 68881, 68882, 68040 or 68060",
                    ),
                }
            }
        }
    }
    if let Some(e) = by_key("mmu").filter(|e| fs_value(&e.value) != "auto") {
        match state.as_mut() {
            None => report.unsupported(&e.key, &e.value, NO_MACHINE_CPU),
            Some(cpu) => set_mmu(cpu, e, report),
        }
    }

    for e in entries.iter().filter(is_uae_cpu_key) {
        let Some(cpu) = state.as_mut() else {
            report.unsupported(&e.key, &e.value, NO_MACHINE_CPU);
            continue;
        };
        match e.key.as_str() {
            // A new core is no longer the EC part a `cpu` value named.
            "uae_cpu_model" => match leading_number(&e.value).filter(|n| CPU_CORES.contains(n)) {
                Some(core) => {
                    cpu.ec &= core == cpu.core;
                    cpu.core = core;
                    cpu.fpu = false;
                    width_entry = Some(e);
                }
                None => report.unsupported(
                    &e.key,
                    &e.value,
                    "unrecognized CPU model; UAE reads only the core's number (68000 to 68060)",
                ),
            },
            "uae_cpu_type" => match parse_cpu_type(&e.value) {
                Some((core, narrow, fpu)) => {
                    cpu.ec &= core == cpu.core;
                    cpu.core = core;
                    cpu.narrow = narrow.unwrap_or(core < 68020);
                    cpu.fpu = fpu;
                    width_entry = Some(e);
                }
                None => report.unsupported(&e.key, &e.value, "unrecognized CPU type"),
            },
            "uae_fpu_model" => {
                let lower = e.value.trim().to_ascii_lowercase();
                let on = !(lower.is_empty() || lower == "0" || lower == "none");
                set_fpu(cpu, on, e, report);
                fpu_entry = Some(e);
            }
            "uae_mmu_model" => set_mmu(cpu, e, report),
            _ => match parse_bool(&e.value) {
                Some(narrow) => {
                    cpu.narrow = narrow;
                    width_entry = Some(e);
                }
                None => report.unsupported(&e.key, &e.value, "unrecognized boolean"),
            },
        }
    }

    let cpu = state?;
    let model = cpu.model();
    // A named CPU is always said; otherwise only when the file does not
    // already say it: a recognized preset has written its machine's model,
    // and without one Copperline's own is the 68000.
    let written = doc
        .get("cpu")
        .and_then(|cpu| cpu.get("model"))
        .and_then(|model| model.as_str())
        .unwrap_or("68000");
    if cpu_named || written != model {
        set_str(doc, &["cpu"], "model", model);
    }
    if let Some(e) = width_entry {
        if let Some(note) = addressing_note(cpu.core, cpu.narrow) {
            report.approximated(&e.key, &e.value, note);
        }
    }
    // The `fpu` option is checked against the core above, so only a raw
    // uae_fpu_model can ask a 68000/68010 for one.
    if let Some(e) = fpu_entry.filter(|_| cpu.fpu && !can_fit_fpu(model)) {
        report.unsupported(
            &e.key,
            &e.value,
            "a 68000/68010 has no coprocessor interface for a 68881/68882; Copperline cannot \
             fit one",
        );
    }
    set_cpu_fpu(doc, model, cpu.fpu, fpu_entry.is_some());
    Some(model)
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
    if by_key("amiga_model").is_none() {
        report.note(
            "the source config named no machine model (amiga_model), so the machine is whatever Copperline defaults to -- currently a stock A500 (68000 at 7.09MHz, 512K chip plus 512K slow, ECS Agnus with OCS Denise, PAL). Set [machine] profile if you wanted something else; the CPU/chipset/memory keys that did translate still override it either way.",
        );
    }
    // The machine before any explicit key: FS-UAE's own default A500 when
    // no model is named, the preset otherwise, and unknown for a preset
    // this mapper does not recognize.
    let model_entry = by_key("amiga_model");
    let preset = match model_entry {
        Some(e) => model_preset(&e.value),
        None => model_preset("A500"),
    };
    // An `accelerator` option, else the preset's board; FS-UAE takes the
    // board's CPU when no `cpu` option names one.
    let board_entry = by_key("accelerator");
    let board = match board_entry {
        Some(e) => {
            seen.insert(&e.key, ());
            let board = accelerator_board(&e.value);
            if board.is_none() {
                report.unsupported(
                    &e.key,
                    &e.value,
                    "Copperline models no PowerPC board, and no accelerator board as such; \
                     set [cpu] and [memory] accelerator by hand",
                );
            }
            board
        }
        None => preset
            .and_then(|p| p.accelerator)
            .and_then(accelerator_board),
    };
    let machine = machine_cpu(preset, board.map(|(_, cpu, _)| cpu));
    if let Some(e) = model_entry {
        seen.insert(&e.key, ());
        match preset {
            Some(p) => {
                // The profile carries the machine's own wiring (the IDE
                // port an A1200/A600/A4000 has, the RTC socket, and the
                // rest); without it a `hard_drive_N` mapped onto [ide]
                // below has no controller to attach to. The explicit
                // chipset/CPU/memory set here still override whatever the
                // profile would have supplied.
                set_str(&mut doc, &["machine"], "profile", p.profile);
                set_str(&mut doc, &["chipset"], "revision", p.chipset);
                if let Some(cpu) = machine.board.or(machine.base) {
                    set_str(&mut doc, &["cpu"], "model", cpu.model());
                }
                set_str(&mut doc, &["memory"], "chip", p.chip);
                annotate(
                    &mut doc,
                    &["chipset"],
                    "revision",
                    &format!(
                        "derived from amiga_model={} -- verify against your source config",
                        e.value
                    ),
                );
                if p.fmv {
                    doc["fmv"] = toml_edit::value(true);
                    report.approximated(
                        &e.key,
                        &e.value,
                        "FS-UAE fits Commodore's FMV module ROM from its Kickstarts folder; \
                         fmv = true fits Copperline's bundled open replacement ROM (set \
                         fmv_rom to the Commodore image to use that instead)",
                    );
                }
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
    let cpu_model = map_cpu(entries, &mut seen, &mut doc, &mut report, machine);

    // --- RAM only a 32-bit CPU reaches ------------------------------------
    // Ramsey motherboard RAM (FS-UAE's `motherboard_ram`, in KB like its
    // other memory options, or the preset's) and an accelerator board's RAM
    // (`accelerator_memory`, also KB, or the board's own) both sit above
    // the 24-bit address space; FS-UAE refuses motherboard RAM without
    // 32-bit addressing, and Copperline refuses both.
    let wide = cpu_model.is_some_and(|m| !matches!(m, "68000" | "68010" | "68EC020"));
    let ramsey = preset
        .map(|p| p.profile)
        .filter(|profile| matches!(*profile, "A3000" | "A4000"));
    let mut motherboard = preset
        .and_then(|p| p.motherboard)
        .map(|size| (size.to_string(), model_entry));
    if let Some(e) = by_key("motherboard_ram") {
        seen.insert(&e.key, ());
        let kb = e.value.trim().parse::<u64>().ok();
        match kb.filter(|kb| kb.is_multiple_of(1024)) {
            None => report.unsupported(
                &e.key,
                &e.value,
                "expected a KB integer, a whole number of megabytes",
            ),
            Some(kb) if ramsey.is_some_and(|p| !ramsey_takes(kb / 1024, p == "A4000")) => report
                .unsupported(
                    &e.key,
                    &e.value,
                    format!(
                        "the {}'s Ramsey takes 1M-4M of motherboard RAM in 1M steps or 8M, 12M \
                         or 16M (the A4000 also 20M-64M in 4M steps); its preset size was kept",
                        ramsey.unwrap_or_default()
                    ),
                ),
            // Said even when zero: the A3000/A4000 profiles fit some.
            Some(0) => motherboard = Some(("0".to_string(), Some(e))),
            Some(kb) => motherboard = Some((format!("{}M", kb / 1024), Some(e))),
        }
    }
    if let Some((size, source)) = motherboard {
        let ramsey = ramsey.is_some();
        if ramsey && (wide || size == "0") {
            set_str(&mut doc, &["memory"], "motherboard", &size);
        } else if let Some(e) = source.filter(|_| size != "0") {
            report.unsupported(
                &e.key,
                &e.value,
                if ramsey {
                    "motherboard RAM needs a CPU with 32-bit addressing"
                } else {
                    "Copperline fits motherboard RAM only to the A3000/A4000's Ramsey controller"
                },
            );
        }
    }
    let accel_entry = by_key("accelerator_memory");
    if let Some(e) = accel_entry {
        seen.insert(&e.key, ());
    }
    if let Some((name, _, board_mb)) = board {
        let mut mb = board_mb;
        if let Some(e) = accel_entry {
            match e.value.trim().parse::<u64>() {
                Ok(kb) => mb = kb / 1024,
                Err(_) => report.unsupported(&e.key, &e.value, "expected a KB integer"),
            }
        }
        // No board RAM is FS-UAE's way of fitting no board at all.
        if let Some(e) = board_entry.or(model_entry).filter(|_| mb > 0) {
            if !wide {
                report.unsupported(
                    &e.key,
                    &e.value,
                    format!("the {name} board's RAM needs a CPU with 32-bit addressing"),
                );
            } else if mb > 128 {
                report.unsupported(
                    &e.key,
                    &e.value,
                    format!("{mb}M of board RAM is beyond Copperline's 128M CPU-slot space"),
                );
            } else {
                set_str(&mut doc, &["memory"], "accelerator", &format!("{mb}M"));
                report.approximated(
                    &e.key,
                    &e.value,
                    format!(
                        "Copperline does not model the {name} board: its CPU and board RAM \
                         are imported (the RAM as [memory] accelerator, Copperline's CPU-slot \
                         RAM), but not the board's own ROM or SCSI kit"
                    ),
                );
            }
        }
    } else if let Some(e) = accel_entry {
        report.unsupported(
            &e.key,
            &e.value,
            "board RAM for an accelerator, and no board this converter maps is fitted",
        );
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
    // `floppy_drive_N_sounds`: a sample set from FS-UAE's "Floppy Sounds"
    // folder, or "off"; unset (or empty) is FS-UAE's built-in sounds.
    let mut drive_sounds = Vec::new();
    for n in 0..4 {
        let Some(e) = by_key(&format!("floppy_drive_{n}_sounds")) else {
            continue;
        };
        seen.insert(&e.key, ());
        let value = e.value.trim();
        let choice = if value.is_empty() {
            DriveSound::BuiltIn
        } else if value.eq_ignore_ascii_case("off") {
            DriveSound::Off
        } else {
            DriveSound::Set(value.to_string())
        };
        drive_sounds.push((e.key.clone(), e.value.clone(), choice));
    }
    apply_drive_sounds(&mut doc, &mut report, &drive_sounds);

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
    let mut copperhf_next = 0usize;
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
        // the Amiga can see. Copperline's [copperhf] (copperhf.device) is
        // the exact analogue of that same virtual controller, so these
        // drives go there, unit for unit in the order they're encountered
        // -- a like-for-like translation, not a substitution onto real
        // hardware the source never had.
        let named = controller.map(|c| c.value.trim().to_ascii_lowercase());
        let virtual_controller = matches!(named.as_deref(), None | Some("") | Some("uae"));
        if virtual_controller {
            if copperhf_next <= 6 {
                let unit_key = format!("unit{copperhf_next}");
                set_str(&mut doc, &["copperhf"], &unit_key, path);
                annotate(
                    &mut doc,
                    &["copperhf"],
                    &unit_key,
                    &format!(
                        "from {} -- FS-UAE served this drive through its own uaehf.device \
                         virtual controller; Copperline's [copperhf] (copperhf.device) is the \
                         exact analogue, so this is an exact translation",
                        e.key
                    ),
                );
                copperhf_next += 1;
            } else {
                report.approximated(
                    &e.key,
                    &e.value,
                    "Copperline's [copperhf] has only seven units (0-6), all already used by \
                     earlier virtual-controller drives; attach this one to [scsi] or [lide] by \
                     hand",
                );
            }
            continue;
        }
        let scsi = named.as_deref().is_some_and(|c| c.starts_with("scsi"));
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
    fn floppy_drive_sounds_name_a_sample_set_or_turn_the_sounds_off() {
        let out = convert("floppy_drive_0_sounds = A500Real\n");
        assert!(out.contains(r#"floppy_sounds_set = "A500Real""#), "{out}");
        let out = convert("floppy_drive_0_sounds = off\n");
        assert!(out.contains("floppy_sounds = false"), "{out}");
        assert!(!out.contains("floppy_sounds_set"), "{out}");
        // A set on DF1 alone still applies: it is the first drive that
        // says anything.
        let out = convert("floppy_drive_1_sounds = ST\n");
        assert!(out.contains(r#"floppy_sounds_set = "ST""#), "{out}");
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
    fn an_unnamed_controller_is_the_virtual_one_and_maps_exactly_onto_copperhf() {
        // FS-UAE's default is its own uaehf.device, not whichever bus the
        // machine has -- but Copperline's [copperhf] (copperhf.device) is
        // the exact analogue of that same virtual controller, so this is a
        // like-for-like translation, not a substitution: nothing lands in
        // the flagged report, and the machine profile is irrelevant to
        // where the drive goes.
        let entries = crate::parse::parse("amiga_model = A1200\nhard_drive_1 = data.hdf\n");
        let out = map(&entries);
        assert!(
            out.doc.to_string().contains(r#"unit0 = "data.hdf""#),
            "{}",
            out.doc
        );
        assert!(!out.doc.to_string().contains("slave ="), "{}", out.doc);
        assert!(out.report.flagged.is_empty(), "{:?}", out.report.flagged);
        assert!(
            out.doc.to_string().contains("copperhf.device"),
            "{}",
            out.doc
        );

        // Same result regardless of machine model -- [copperhf] needs no
        // board, no ROM, and no particular IDE/SCSI port to exist on.
        let out = convert("amiga_model = A3000\nhard_drive_0 = os.hdf\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
        assert!(!out.contains("master ="), "{out}");

        let out = convert("hard_drive_0 = os.hdf\n");
        assert!(out.contains(r#"unit0 = "os.hdf""#), "{out}");
        assert!(!out.contains("master ="), "{out}");
    }

    #[test]
    fn successive_virtual_controller_drives_take_successive_copperhf_units() {
        let out = convert(
            "hard_drive_0 = a.hdf\nhard_drive_1 = b.hdf\nhard_drive_2 = c.hdf\n\
             hard_drive_2_controller = scsi\n",
        );
        assert!(out.contains(r#"unit0 = "a.hdf""#), "{out}");
        assert!(out.contains(r#"unit1 = "b.hdf""#), "{out}");
        // An explicit non-virtual controller does not consume a copperhf
        // unit -- it goes straight to [scsi] under its own drive index.
        assert!(out.contains(r#"unit2 = "c.hdf""#), "{out}");
        assert!(!out.contains("unit3 ="), "{out}");
    }

    #[test]
    fn more_than_seven_virtual_controller_drives_are_flagged_past_the_ceiling() {
        let mut src = String::new();
        for i in 0..8u32 {
            src.push_str(&format!("hard_drive_{i} = d{i}.hdf\n"));
        }
        let entries = crate::parse::parse(&src);
        let out = map(&entries);
        for i in 0..7u32 {
            assert!(
                out.doc
                    .to_string()
                    .contains(&format!(r#"unit{i} = "d{i}.hdf""#)),
                "{}",
                out.doc
            );
        }
        assert!(!out.doc.to_string().contains("d7.hdf"), "{}", out.doc);
        assert_eq!(out.report.flagged.len(), 1);
        assert!(out.report.flagged[0].note.contains("seven units"));
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

        // ...and a real image under such a parent still is one -- landing
        // on [copperhf] (the default virtual controller), not [ide].
        let entries = crate::parse::parse("hard_drive_0 = /home/me/.fs-uae/hd/System.hdf\n");
        let out = map(&entries);
        assert!(out.doc.to_string().contains("unit0 ="), "{}", out.doc);
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

    /// `convert`, then checked the way Copperline loads a config, so a
    /// mapping can never hand the user a file it would refuse.
    fn convert_valid(text: &str) -> String {
        let out = convert(text);
        let raw = copperline::config::RawConfig::parse(&out).expect("valid TOML");
        if let Err(e) = copperline::config::Config::try_from(raw) {
            panic!("{out}\nis refused: {e:#}");
        }
        out
    }

    fn flagged_keys(text: &str) -> Vec<String> {
        let entries = crate::parse::parse(text);
        map(&entries)
            .report
            .flagged
            .into_iter()
            .map(|f| f.source_key)
            .collect()
    }

    #[test]
    fn the_cpu_option_names_every_ec_and_lc_part() {
        // FS-UAE's doc/options/cpu values; the LC parts are the full
        // 68040/68060 without their FPU.
        for (value, model, fpu) in [
            ("68000", "68000", None),
            ("68010", "68010", None),
            ("68EC020", "68EC020", None),
            ("68020", "68020", None),
            ("68EC030", "68EC030", None),
            ("68030", "68030", None),
            ("68EC040", "68EC040", None),
            ("68LC040", "68040", Some(false)),
            ("68040", "68040", None),
            ("68EC060", "68EC060", None),
            ("68LC060", "68060", Some(false)),
            ("68060", "68060", None),
        ] {
            let text = format!("cpu = {value}\n");
            let out = convert_valid(&text);
            assert!(out.contains(&format!(r#"model = "{model}""#)), "{out}");
            match fpu {
                Some(fpu) => assert!(out.contains(&format!("fpu = {fpu}")), "{out}"),
                None => assert!(!out.contains("fpu ="), "{out}"),
            }
            assert!(flagged_keys(&text).is_empty(), "{value}");
        }
        // FS-UAE compares values ignoring case and punctuation.
        assert!(convert("cpu = 68ec030\n").contains(r#"model = "68EC030""#));
        assert!(convert("cpu = 68lc_040\n").contains(r#"model = "68040""#));
        // `cpu_type` is not an FS-UAE option (FS-UAE's own is `cpu`).
        assert!(!convert("cpu_type = 68030\n").contains("model ="));
    }

    #[test]
    fn a_nommu_cpu_is_the_full_part_and_says_so() {
        // No real 68040/68060 has its FPU without the MMU: FS-UAE's -NOMMU
        // is the full part with MMU emulation switched off.
        for core in ["68040", "68060"] {
            let text = format!("cpu = {core}-NOMMU\n");
            let out = convert_valid(&text);
            assert!(out.contains(&format!(r#"model = "{core}""#)), "{out}");
            assert!(!out.contains("fpu ="), "{out}");
            assert_eq!(flagged_keys(&text), ["cpu"]);
        }
    }

    #[test]
    fn fpu_and_mmu_options_adjust_the_part_as_fsuae_does() {
        let out = convert_valid("cpu = 68EC030\nfpu = 68882\n");
        assert!(out.contains(r#"model = "68EC030""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        let out = convert_valid("cpu = 68LC040\nfpu = 68040\n");
        assert!(out.contains(r#"model = "68040""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        let out = convert_valid("cpu = 68060\nfpu = 0\n");
        assert!(out.contains("fpu = false"), "{out}");
        // An MMU added to an EC part makes the full (or LC) part.
        let out = convert_valid("cpu = 68EC030\nmmu = 68030\n");
        assert!(out.contains(r#"model = "68030""#), "{out}");
        let out = convert_valid("cpu = 68EC040\nmmu = 68040\n");
        assert!(out.contains(r#"model = "68040""#), "{out}");
        assert!(out.contains("fpu = false"), "{out}");
        // The FPU fits the preset's CPU when no `cpu` is named.
        let out = convert_valid("amiga_model = A1200\nfpu = 68881\n");
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        // The on-chip FPU on an EC part is FS-UAE's -NOMMU CPU.
        let text = "cpu = 68EC040\nfpu = 68040\n";
        assert!(convert_valid(text).contains(r#"model = "68040""#));
        assert_eq!(flagged_keys(text), ["fpu"]);
        // `mmu = 0` leaves the part alone, but says Copperline's has one.
        let text = "cpu = 68030\nmmu = 0\n";
        assert!(convert_valid(text).contains(r#"model = "68030""#));
        assert_eq!(flagged_keys(text), ["mmu"]);
        assert!(flagged_keys("cpu = 68EC030\nmmu = 0\n").is_empty());
    }

    #[test]
    fn fpu_and_mmu_options_fsuae_ignores_are_reported_not_emitted() {
        // FS-UAE warns about and ignores an FPU or MMU that does not fit
        // the core, so the import does too.
        let text = "cpu = 68EC040\nfpu = 68881\n";
        let out = convert_valid(text);
        assert!(out.contains(r#"model = "68EC040""#), "{out}");
        assert!(!out.contains("fpu ="), "{out}");
        assert_eq!(flagged_keys(text), ["fpu"]);
        // The default A500's 68000 has no coprocessor interface.
        let text = "fpu = 68881\n";
        assert!(!convert_valid(text).contains("fpu ="));
        assert_eq!(flagged_keys(text), ["fpu"]);
        let text = "cpu = 68030\nmmu = 68040\n";
        assert!(convert_valid(text).contains(r#"model = "68030""#));
        assert_eq!(flagged_keys(text), ["mmu"]);
        // An unknown CPU keeps the machine's, as FS-UAE does.
        let text = "cpu = 68EC031\n";
        assert!(!convert_valid(text).contains("model ="));
        assert_eq!(flagged_keys(text), ["cpu"]);
        // With a preset this converter does not know, there is no CPU to
        // check an FPU against.
        let text = "amiga_model = A4000/PPC\nfpu = 68881\nuae_cpu_model = 68030\n";
        assert!(!convert(text).contains("fpu ="));
        assert_eq!(flagged_keys(text), ["amiga_model", "fpu", "uae_cpu_model"]);
    }

    #[test]
    fn uae_cpu_options_apply_after_fsuae_s_own_in_file_order() {
        // FS-UAE hands uae_* lines to UAE's parser after its own options.
        let out = convert_valid("cpu = 68030\nuae_cpu_model = 68040\nuae_fpu_model = 68040\n");
        assert!(out.contains(r#"model = "68040""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        // UAE clears the FPU with every cpu_model line, so the order of the
        // lines decides whether the FPU survives.
        let out = convert_valid("cpu = 68030\nuae_fpu_model = 68882\nuae_cpu_model = 68030\n");
        assert!(out.contains("fpu = false"), "{out}");
        let out = convert_valid("cpu = 68030\nuae_cpu_model = 68030\nuae_fpu_model = 68882\n");
        assert!(out.contains("fpu = true"), "{out}");
        // A cpu_model line leaves the address width alone: the A500's 24-bit
        // 68000 becomes a 68EC020, until 32-bit addressing is asked for.
        let out = convert_valid("uae_cpu_model = 68020\n");
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        let out = convert_valid("uae_cpu_model = 68020\nuae_cpu_24bit_addressing = false\n");
        assert!(out.contains(r#"model = "68020""#), "{out}");
        let out = convert_valid("amiga_model = A1200\nuae_cpu_24bit_addressing = false\n");
        assert!(out.contains(r#"model = "68020""#), "{out}");
        // An MMU switched on makes the full part; an FPU-less 68040 with
        // its MMU is the LC040.
        let out = convert_valid("cpu = 68EC040\nuae_mmu_model = 68040\n");
        assert!(out.contains(r#"model = "68040""#), "{out}");
        assert!(out.contains("fpu = false"), "{out}");
        // The deprecated old-style type still reads.
        let out = convert_valid("uae_cpu_type = 68ec020/68881\n");
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
    }

    #[test]
    fn uae_cpu_options_copperline_cannot_take_are_reported_not_emitted() {
        // FS-UAE's UAE core reads mmu_model as a number: no EC MMU.
        let text = "cpu = 68030\nuae_mmu_model = 68ec030\n";
        assert!(convert_valid(text).contains(r#"model = "68030""#));
        assert_eq!(flagged_keys(text), ["uae_mmu_model"]);
        let text = "cpu = 68030\nuae_mmu_model = 68040\n";
        assert!(convert_valid(text).contains(r#"model = "68030""#));
        assert_eq!(flagged_keys(text), ["uae_mmu_model"]);
        // A 6888x on a 68000.
        let text = "uae_fpu_model = 68881\n";
        assert!(!convert_valid(text).contains("fpu ="));
        assert_eq!(flagged_keys(text), ["uae_fpu_model"]);
        // An on-chip FPU on an EC part is the -NOMMU CPU: the full part.
        let text = "cpu = 68EC060\nuae_fpu_model = 68060\n";
        assert!(convert_valid(text).contains(r#"model = "68060""#));
        assert_eq!(flagged_keys(text), ["uae_fpu_model"]);
        // Address widths the Copperline part does not have.
        let text = "uae_cpu_24bit_addressing = false\n";
        assert!(!convert_valid(text).contains("model ="));
        assert_eq!(flagged_keys(text), ["uae_cpu_24bit_addressing"]);
        let text = "uae_cpu_model = 68030\n";
        assert!(convert_valid(text).contains(r#"model = "68030""#));
        assert_eq!(flagged_keys(text), ["uae_cpu_model"]);
        let text = "uae_cpu_model = 68EC030\n";
        assert!(!convert_valid(text).contains("model ="));
        assert_eq!(flagged_keys(text), ["uae_cpu_model"]);
    }

    #[test]
    fn big_box_presets_bring_their_68882_and_motherboard_ram() {
        // bip_a3000/bip_a4000: a 68882 and 8M of Ramsey motherboard RAM,
        // not a Zorro II board; FS-UAE's A4000 is the 68EC030 one.
        let out = convert_valid("amiga_model = A3000\n");
        assert!(out.contains(r#"model = "68030""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        assert!(out.contains(r#"motherboard = "8M""#), "{out}");
        assert!(!out.contains("fast ="), "{out}");
        let out = convert_valid("amiga_model = A4000\n");
        assert!(out.contains(r#"model = "68EC030""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        assert!(out.contains(r#"motherboard = "8M""#), "{out}");
        let out = convert_valid("amiga_model = A4000/040\n");
        assert!(out.contains(r#"model = "68040""#), "{out}");
        assert!(!out.contains("fpu ="), "{out}");
        // A named CPU keeps the board's 68882 beside it (FS-UAE's
        // default_fpu_noninternal), or drops it for an EC part.
        let out = convert_valid("amiga_model = A4000\ncpu = 68030\n");
        assert!(out.contains(r#"model = "68030""#), "{out}");
        assert!(out.contains("fpu = true"), "{out}");
        let out = convert_valid("amiga_model = A3000\ncpu = 68EC030\n");
        assert!(out.contains(r#"model = "68EC030""#), "{out}");
        assert!(!out.contains("fpu ="), "{out}");
        // An explicit size replaces the preset's; a 24-bit CPU reaches none.
        let out = convert_valid("amiga_model = A4000\nmotherboard_ram = 16384\n");
        assert!(out.contains(r#"motherboard = "16M""#), "{out}");
        let out = convert_valid("amiga_model = A4000\nmotherboard_ram = 0\n");
        assert!(out.contains(r#"motherboard = "0""#), "{out}");
        // (Copperline refuses an A3000 with a 68000 whatever the import
        // does: the profile's own motherboard RAM and Zorro III RTG card
        // need 32 address lines too.)
        let text = "amiga_model = A3000\ncpu = 68000\n";
        assert!(!convert(text).contains("motherboard ="));
        assert_eq!(flagged_keys(text), ["amiga_model"]);
        let text = "amiga_model = A1200\nmotherboard_ram = 8192\n";
        assert!(!convert_valid(text).contains("motherboard ="));
        assert_eq!(flagged_keys(text), ["motherboard_ram"]);
    }

    #[test]
    fn a_named_68020_takes_the_machine_s_external_fpu() {
        // configure_cpu gives both 68020 forms default_fpu_noninternal: none
        // beside the A4000/040's on-chip default, the 68882 on an A4000.
        let out = convert_valid("amiga_model = A4000/040\ncpu = 68020\n");
        assert!(out.contains(r#"model = "68020""#), "{out}");
        assert!(!out.contains("fpu ="), "{out}");
        let out = convert_valid("amiga_model = A4000\ncpu = 68020\n");
        assert!(out.contains("fpu = true"), "{out}");
    }

    #[test]
    fn an_accelerator_option_alone_brings_its_cpu() {
        // No amiga_model and no cpu: FS-UAE's A500 takes the board's CPU,
        // which must reach the board's RAM.
        let text = "accelerator = blizzard-1260\n";
        let out = convert_valid(text);
        assert!(out.contains(r#"model = "68060""#), "{out}");
        assert!(out.contains(r#"accelerator = "32M""#), "{out}");
        assert_eq!(flagged_keys(text), ["accelerator"]);
    }

    #[test]
    fn cpu_auto_keeps_the_machine_s_own_cpu_over_its_board() {
        // configure_cpu takes the board's CPU only with no `cpu` option at
        // all; `auto` keeps the model's own, at the model's address width.
        let text = "amiga_model = A1200\naccelerator = blizzard-1260\ncpu = auto\n";
        let out = convert_valid(text);
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        // ...whose 24 address lines cannot reach the board's RAM.
        assert!(!out.contains("accelerator ="), "{out}");
        assert_eq!(flagged_keys(text), ["accelerator"]);
        // The Blizzard presets turn 32-bit addressing on.
        let out = convert_valid("amiga_model = A1200/1260\ncpu = auto\n");
        assert!(out.contains(r#"model = "68020""#), "{out}");
        assert!(out.contains(r#"accelerator = "32M""#), "{out}");
    }

    #[test]
    fn motherboard_ram_must_fill_ramsey_banks() {
        // validate_mb_ram: Ramsey-04 stops at 16M, and no Ramsey takes 5M;
        // the preset's own size stays.
        for text in [
            "amiga_model = A3000\nmotherboard_ram = 32768\n",
            "amiga_model = A4000\nmotherboard_ram = 5120\n",
        ] {
            let out = convert_valid(text);
            assert!(out.contains(r#"motherboard = "8M""#), "{out}");
            assert_eq!(flagged_keys(text), ["motherboard_ram"]);
        }
        // Ramsey-07 extends in 4M steps to 64M.
        let out = convert_valid("amiga_model = A4000\nmotherboard_ram = 32768\n");
        assert!(out.contains(r#"motherboard = "32M""#), "{out}");
        let out = convert_valid("amiga_model = A3000\nmotherboard_ram = 2048\n");
        assert!(out.contains(r#"motherboard = "2M""#), "{out}");
    }

    #[test]
    fn cd32_and_blizzard_presets_map_to_their_parts() {
        let out = convert_valid("amiga_model = CD32\n");
        assert!(out.contains(r#"profile = "CD32""#), "{out}");
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        assert!(!out.contains("fmv"), "{out}");
        let text = "amiga_model = CD32/FMV\n";
        assert!(convert_valid(text).contains("fmv = true"));
        assert_eq!(flagged_keys(text), ["amiga_model"]);
        // config-accelerator.c: each board's CPU and 32M of board RAM, which
        // becomes Copperline's CPU-slot RAM; the board itself is flagged.
        for (model, cpu) in [
            ("A1200/1230", "68EC030"),
            ("A1200/1240", "68040"),
            ("A1200/1260", "68060"),
        ] {
            let text = format!("amiga_model = {model}\n");
            let out = convert_valid(&text);
            assert!(out.contains(r#"profile = "A1200""#), "{out}");
            assert!(out.contains(&format!(r#"model = "{cpu}""#)), "{out}");
            assert!(!out.contains("fpu ="), "{out}");
            assert!(out.contains(r#"accelerator = "32M""#), "{out}");
            assert_eq!(flagged_keys(&text), ["amiga_model"]);
        }
        // A named CPU wins over the board's; the board's RAM needs one with
        // 32 address lines.
        let out =
            convert_valid("amiga_model = A1200/1260\ncpu = 68030\naccelerator_memory = 65536\n");
        assert!(out.contains(r#"model = "68030""#), "{out}");
        assert!(out.contains(r#"accelerator = "64M""#), "{out}");
        let text = "amiga_model = A1200/1230\ncpu = 68EC020\n";
        assert!(!convert_valid(text).contains("accelerator ="));
        assert_eq!(flagged_keys(text), ["amiga_model"]);
        // An `accelerator` option fits the board to any machine; PowerPC
        // boards are not modelled.
        let out = convert_valid("amiga_model = A1200\naccelerator = blizzard-1260\n");
        assert!(out.contains(r#"model = "68060""#), "{out}");
        let text = "amiga_model = A1200\naccelerator = blizzard-ppc\n";
        assert!(convert_valid(text).contains(r#"model = "68EC020""#));
        assert_eq!(flagged_keys(text), ["accelerator"]);
    }

    #[test]
    fn the_plain_a1200_preset_is_a_68ec020() {
        // FS-UAE's doc/options/cpu: "A1200" is a 68EC020, "A1200/020" the
        // full 68020.
        let out = convert_valid("amiga_model = A1200\n");
        assert!(out.contains(r#"model = "68EC020""#), "{out}");
        let out = convert_valid("amiga_model = A1200/020\n");
        assert!(out.contains(r#"model = "68020""#), "{out}");
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
