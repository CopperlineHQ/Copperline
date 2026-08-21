//! ROM identification lines and About-window summaries.
use super::*;
use std::path::Path;
/// What a boot-ROM path holds, in words: the Kickstart version an image
/// checksums to (see [`crate::romdb`]), `"bundled AROS"` for the
/// open-source ROM Copperline ships in place of a Kickstart, or `None` for
/// an image no table entry names.
///
/// This reads the file, so it belongs on the paths that run once -- the
/// start-up banner, the About panel, a ROM chosen in the configuration
/// screen -- and never in a per-frame one.
pub fn rom_identification(path: &Path) -> Option<String> {
    // The sentinel survives when no ROM was named and none was resolved
    // (a save state carries its own ROM image), and the resolved AROS pair
    // is named by its file names: neither is in the checksum table, and
    // both are worth saying out loud rather than leaving blank.
    let aros = path == Path::new(BUNDLED_AROS_ROM)
        || path.file_name().is_some_and(|name| {
            name == crate::romsearch::AROS_MAIN_FILE || name == crate::romsearch::AROS_EXT_FILE
        });
    if aros {
        return Some("bundled AROS".to_string());
    }
    crate::romdb::describe_file(path).map(|id| id.label().to_string())
}

/// The identification the About page prefers: a known checksum's label,
/// or for the bundled AROS the version and revision read off the image
/// itself, the way the launcher's Kickstart row shows them. `None` for
/// an image nothing can name -- the file name carries the line then.
pub fn about_rom_identification(path: &Path) -> Option<String> {
    let id = rom_identification(path)?;
    if id != "bundled AROS" {
        return Some(id);
    }
    match crate::romdb::rom_self_versions(path) {
        Some((version, revision)) if !version.is_empty() => Some(if revision.is_empty() {
            format!("AROS {version}")
        } else {
            format!("AROS {version} ({revision})")
        }),
        _ => Some(id),
    }
}

/// The `ROM:` line the About panel and the ROM-load OSD show: the
/// identification the configuration page shows when the image is a known
/// one, the file's name when it is not.
pub fn about_rom_line(name: &str, identification: Option<&str>) -> String {
    match identification {
        Some(id) => format!("ROM: {id}"),
        None => format!("ROM: {name}"),
    }
}

/// The `Extended ROM:` line, shown only when one is fitted: the same
/// shape as the boot ROM's line.
pub fn about_ext_rom_line(name: &str, identification: Option<&str>) -> String {
    format!("Extended {}", about_rom_line(name, identification))
}

/// The About machine line of the configuration screen, where no
/// machine is fitted yet. The About page recognises it and centres it
/// as an invitation rather than bulleting it as a fact.
pub const ABOUT_PLACEHOLDER_LINE: &str = "Configure a machine, press Run!";

/// Emulated-machine summary lines for the About window.
pub fn about_machine_lines(cfg: &Config) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(machine) = cfg.machine {
        lines.push(format!("Machine: {machine:?}"));
    }
    lines.push(format!("CPU: {:?} @ {} MHz", cfg.cpu, cfg.cpu_clock_mhz));
    lines.push(format!(
        "Chipset: {:?} ({:?}/{:?}, {:?})",
        cfg.chipset, cfg.agnus_revision, cfg.denise_revision, cfg.video_standard
    ));
    let mut ram = format!("RAM: {}K chip", cfg.chip_ram_bytes / 1024);
    if cfg.slow_ram_bytes > 0 {
        ram.push_str(&format!(", {}K slow", cfg.slow_ram_bytes / 1024));
    }
    if cfg.fast_ram_bytes > 0 {
        ram.push_str(&format!(", {}K fast", cfg.fast_ram_bytes / 1024));
    }
    if cfg.mb_ram_bytes > 0 {
        ram.push_str(&format!(", {}K motherboard", cfg.mb_ram_bytes / 1024));
    }
    if cfg.accel_ram_bytes > 0 {
        ram.push_str(&format!(", {}K accelerator", cfg.accel_ram_bytes / 1024));
    }
    if cfg.z3_ram_bytes > 0 {
        ram.push_str(&format!(", {}K Z3", cfg.z3_ram_bytes / 1024));
    }
    lines.push(ram);
    if let Some(name) = cfg.rom_path.file_name() {
        // The file name is whatever the dumper called it; the identification
        // says which Kickstart it actually is.
        lines.push(about_rom_line(
            &name.to_string_lossy(),
            about_rom_identification(&cfg.rom_path).as_deref(),
        ));
    }
    if let Some(ext) = cfg.extended_rom_path.as_deref() {
        if let Some(name) = ext.file_name() {
            lines.push(about_ext_rom_line(
                &name.to_string_lossy(),
                about_rom_identification(ext).as_deref(),
            ));
        }
    }
    let drives = cfg
        .floppy_connected
        .iter()
        .filter(|&&connected| connected)
        .count();
    lines.push(format!("Floppy drives: {drives}"));
    lines
}
