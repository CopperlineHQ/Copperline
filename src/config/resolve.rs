//! Environment-variable overrides applied on top of a loaded config.
use super::*;
use anyhow::Result;
use std::path::PathBuf;
/// Resolve the phosphor persistence fraction: the `COPPERLINE_PHOSPHOR`
/// env var (0.0..=0.95) overrides the `[display] phosphor` config for one
/// run.
pub fn resolve_phosphor(from_config: f32) -> f32 {
    match crate::envcfg::var("COPPERLINE_PHOSPHOR") {
        Some(v) => match v.trim().parse::<f32>() {
            Ok(p) if (0.0..=0.95).contains(&p) => p,
            _ => {
                log::warn!(
                    "COPPERLINE_PHOSPHOR must be between 0.0 and 0.95, got {v:?}; using config value"
                );
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve deinterlacing: the `COPPERLINE_DEINTERLACE` env var overrides
/// the `[display] deinterlace` config for one run (any value other than
/// 0/false/off/no enables it).
pub fn resolve_deinterlace(from_config: bool) -> bool {
    match crate::envcfg::var("COPPERLINE_DEINTERLACE") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => from_config,
    }
}

/// Resolve the presented overscan mode: the `COPPERLINE_OVERSCAN` env var
/// (full/tv) overrides the `[display] overscan` config for one run. The
/// image-regression harness pins "full" so its baselines always carry the
/// whole overscan field regardless of the config default.
pub fn resolve_overscan(from_config: Overscan) -> Overscan {
    match crate::envcfg::var("COPPERLINE_OVERSCAN") {
        Some(v) => match parse_overscan(&v) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_OVERSCAN: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the presentation pixel aspect: the `COPPERLINE_PIXEL_ASPECT`
/// env var (tv/square) overrides the `[display] pixel_aspect` config for
/// one run, so headless A/B captures can pin a mode without editing the
/// config.
pub fn resolve_pixel_aspect(from_config: PixelAspect) -> PixelAspect {
    match crate::envcfg::var("COPPERLINE_PIXEL_ASPECT") {
        Some(v) => match parse_pixel_aspect(&v) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_PIXEL_ASPECT: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the window shader pass: the `COPPERLINE_SHADER` env var (a
/// preset name or a `.wgsl` path) overrides the `[display] shader` config
/// for one run.
pub fn resolve_shader(from_config: ShaderMode) -> ShaderMode {
    match crate::envcfg::var("COPPERLINE_SHADER") {
        Some(v) => match parse_shader(&v) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_SHADER: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the shader mix: the `COPPERLINE_SHADER_STRENGTH` env var
/// (0.0..=1.0) overrides the `[display] shader_strength` config for one
/// run.
pub fn resolve_shader_strength(from_config: f32) -> f32 {
    match crate::envcfg::var("COPPERLINE_SHADER_STRENGTH") {
        Some(v) => match v.trim().parse::<f32>() {
            Ok(p) if (0.0..=1.0).contains(&p) => p,
            _ => {
                log::warn!(
                    "COPPERLINE_SHADER_STRENGTH must be between 0.0 and 1.0, got {v:?}; using config value"
                );
                from_config
            }
        },
        None => from_config,
    }
}

/// Resolve the monitor bezel: the `COPPERLINE_BEZEL` env var (a style name,
/// or the 0/1-style on-off spellings it took when there was only one frame)
/// overrides the `[display] bezel` config for one run. An unreadable value
/// leaves the config's choice alone.
pub fn resolve_bezel(from_config: BezelStyle) -> BezelStyle {
    let Some(v) = crate::envcfg::var("COPPERLINE_BEZEL") else {
        return from_config;
    };
    match v.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" | "none" => BezelStyle::None,
        "1" | "true" | "on" | "yes" => BezelStyle::Model1084,
        other => parse_bezel(other).unwrap_or(from_config),
    }
}

/// Resolve the bezel sticker folder: the `COPPERLINE_BEZEL_STICKERS` env
/// var overrides the `[display] bezel_stickers` config for one run; an
/// empty value disables stickers the config turned on.
pub fn resolve_bezel_stickers(from_config: Option<PathBuf>) -> Option<PathBuf> {
    match crate::envcfg::var("COPPERLINE_BEZEL_STICKERS") {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(PathBuf::from(v.trim())),
        None => from_config,
    }
}

/// Resolve the performance overlay: the `COPPERLINE_PERF_OVERLAY` env var
/// (0/false/off/no disables, anything else enables) overrides the
/// `[display] perf_overlay` config for one run.
pub fn resolve_perf_overlay(from_config: bool) -> bool {
    match crate::envcfg::var("COPPERLINE_PERF_OVERLAY") {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => from_config,
    }
}

/// Resolve the screen tint: the `COPPERLINE_TINT` env var (a tint name)
/// overrides the `[display] tint` config for one run.
pub fn resolve_tint(from_config: Tint) -> Tint {
    match crate::envcfg::var("COPPERLINE_TINT") {
        Some(v) => match parse_tint(&v) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("ignoring COPPERLINE_TINT: {e}");
                from_config
            }
        },
        None => from_config,
    }
}

/// Substitute the bundled AROS ROM when the user named no ROM. The default
/// `rom_path` is a sentinel ([`BUNDLED_AROS_ROM`]); any real path from
/// `rom = "..."` or the CLI argument replaces it before this runs and is left
/// untouched. When the sentinel survives, locate the bundled AROS main +
/// extended ROM pair and rewrite the config to point at them, so every
/// downstream consumer (start-up banner, window title, save states) sees the
/// real paths. An explicit `extended_rom` still wins over the AROS one.
pub fn resolve_bundled_rom(cfg: &mut Config) -> Result<()> {
    // An A4091 may want its bundled ROM regardless of what Kickstart is in use.
    resolve_bundled_a2091_rom(cfg)?;
    resolve_bundled_a4091_rom(cfg)?;
    resolve_bundled_lide_rom(cfg)?;
    resolve_bundled_fmv_rom(cfg)?;
    if cfg.rom_path != Path::new(BUNDLED_AROS_ROM) {
        return Ok(());
    }
    let aros = crate::romsearch::find_bundled_aros().ok_or_else(|| {
        anyhow!(
            "no ROM specified and the bundled AROS ROM was not found. Pass a \
             Kickstart ROM (as the first argument or rom = \"...\" in a config), \
             or install the AROS files ({} and {}) next to the binary or under \
             share/copperline/aros/.",
            crate::romsearch::AROS_MAIN_FILE,
            crate::romsearch::AROS_EXT_FILE
        )
    })?;
    log::info!(
        "no ROM specified; booting bundled AROS ({})",
        aros.main.display()
    );
    cfg.rom_path = aros.main;
    cfg.extended_rom_path.get_or_insert(aros.extended);
    Ok(())
}

/// Resolve a [`BUNDLED_A2091_ROM`] sentinel to Copperline's bundled open
/// A2091/A590 autoboot ROM.
fn resolve_bundled_a2091_rom(cfg: &mut Config) -> Result<()> {
    if cfg.scsi.rom.as_deref() != Some(Path::new(BUNDLED_A2091_ROM)) {
        return Ok(());
    }
    let rom = crate::romsearch::find_bundled_a2091().ok_or_else(|| {
        anyhow!(
            "[scsi] controller = \"a2091\" but the bundled open ROM was not \
             found. Set [scsi] rom = \"...\" (and rom_odd for split EPROMs), \
             or install {} next to the binary or under \
             share/copperline/a2091/.",
            crate::romsearch::A2091_ROM_FILE
        )
    })?;
    log::info!(
        "no A2091 ROM specified; using bundled open ROM ({})",
        rom.display()
    );
    cfg.scsi.rom = Some(rom);
    Ok(())
}

/// Resolve the CD32 profile's bundled open FMV cartridge ROM. Keeping this
/// beside the other bundled-ROM resolution makes save states, About text, and
/// the machine builder all see the concrete installed path.
fn resolve_bundled_fmv_rom(cfg: &mut Config) -> Result<()> {
    if cfg.fmv_rom_path.as_deref() != Some(Path::new(BUNDLED_FMV_ROM)) {
        return Ok(());
    }
    let rom = crate::romsearch::find_bundled_fmv().ok_or_else(|| {
        anyhow!(
            "the CD32 profile's bundled FMV ROM was not found. Set fmv_rom = \"\" \
             to leave the module unfitted, name another 256 KiB FMV ROM, or install \
             {} next to the binary or under share/copperline/fmv/.",
            crate::romsearch::FMV_ROM_FILE
        )
    })?;
    log::info!("using bundled open CD32 FMV ROM ({})", rom.display());
    cfg.fmv_rom_path = Some(rom);
    Ok(())
}

/// Resolve a [`BUNDLED_A4091_ROM`] sentinel in `[scsi] rom` to the located
/// bundled ROM, or fail telling the user where to install one.
fn resolve_bundled_a4091_rom(cfg: &mut Config) -> Result<()> {
    if cfg.scsi.rom.as_deref() != Some(Path::new(BUNDLED_A4091_ROM)) {
        return Ok(());
    }
    let rom = crate::romsearch::find_bundled_a4091().ok_or_else(|| {
        anyhow!(
            "[scsi] controller = \"a4091\" but no ROM was named and the bundled \
             A4091 ROM was not found. Set [scsi] rom = \"...\" (a raw A4091 EPROM \
             image), or install {} next to the binary or under \
             share/copperline/a4091/.",
            crate::romsearch::A4091_ROM_FILE
        )
    })?;
    log::info!(
        "no A4091 ROM specified; using bundled ROM ({})",
        rom.display()
    );
    cfg.scsi.rom = Some(rom);
    Ok(())
}

/// Resolve [`BUNDLED_LIDE_ROM`]/[`BUNDLED_LIDE_CDFS_ROM`] sentinels in
/// `[lide] rom`/`rom_bank2` to the located bundled ROMs, or fail telling the
/// user where to install them. RIPPLE/RIDE resolve to `lide.rom`; AT-Bus
/// 2008 resolves to the separate `lide-atbus.rom` it actually needs (see
/// [`crate::romsearch::LIDE_ATBUS_ROM_FILE`]). A `rom_bank2` sentinel with
/// no bundled `cdfs.rom` installed is simply left unset -- the primary ROM
/// still resolves and the board still autoboots, just without a CD
/// filesystem baked in.
fn resolve_bundled_lide_rom(cfg: &mut Config) -> Result<()> {
    let wants_rom = cfg.lide.rom.as_deref() == Some(Path::new(BUNDLED_LIDE_ROM));
    let wants_cdfs = cfg.lide.rom_bank2.as_deref() == Some(Path::new(BUNDLED_LIDE_CDFS_ROM));
    if !wants_rom && !wants_cdfs {
        return Ok(());
    }
    let lide = crate::romsearch::find_bundled_lide().ok_or_else(|| {
        anyhow!(
            "a [lide] board is fitted but no ROM was named and the bundled lide ROM was not \
             found. Set [lide] rom = \"...\" (a lide.device release's lide.rom), or install \
             {} next to the binary or under share/copperline/lide/ -- or set rom = \"\" to \
             keep the board in hardware-only mode.",
            crate::romsearch::LIDE_ROM_FILE
        )
    })?;
    if wants_rom {
        // AT-Bus 2008 needs its own build (bootloader at offset 0, no
        // header) -- lide.rom's layout does not boot it. Falling back to
        // lide.rom when lide-atbus.rom is not installed would produce a
        // board that fails to autoboot instead of clearly saying why.
        let rom = if cfg.lide.board == crate::ide_zorro::LidePersonality::AtBus2008 {
            lide.atbus.ok_or_else(|| {
                anyhow!(
                    "board = \"atbus2008\" but no ROM was named and the bundled {} was not \
                     found (only {} is installed). Set [lide] rom = \"...\", or install {} \
                     alongside it, or set rom = \"\" for hardware-only mode.",
                    crate::romsearch::LIDE_ATBUS_ROM_FILE,
                    crate::romsearch::LIDE_ROM_FILE,
                    crate::romsearch::LIDE_ATBUS_ROM_FILE
                )
            })?
        } else {
            lide.rom
        };
        log::info!(
            "no lide ROM specified; using bundled ROM ({})",
            rom.display()
        );
        cfg.lide.rom = Some(rom);
    }
    if wants_cdfs {
        match lide.cdfs {
            Some(cdfs) => {
                log::info!(
                    "no lide CD filesystem ROM specified; using bundled ({})",
                    cdfs.display()
                );
                cfg.lide.rom_bank2 = Some(cdfs);
            }
            None => cfg.lide.rom_bank2 = None,
        }
    }
    Ok(())
}
