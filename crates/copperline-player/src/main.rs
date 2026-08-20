// SPDX-License-Identifier: GPL-3.0-or-later

//! The publisher-kit player: a dedicated, launcher-free build of one game.
//!
//! Everything about the machine is baked at compile time from the game
//! manifest (see build.rs); the game payload is a sidecar file beside the
//! binary; per-user settings, saves, and gamepad calibration live in a
//! per-game config directory. There is no debugging surface: the control
//! server and GDB stub are not compiled in, and every `COPPERLINE_*`
//! environment knob is sealed off before anything can read one.

use anyhow::{anyhow, bail, Context, Result};
use copperline::audio::{AudioSink, CpalSink, NullSink};
use copperline::config::{self, Config, RawConfig};
use copperline::video::window::App;
use copperline::{emulator, envcfg, paths, runprog, video};
use std::path::{Path, PathBuf};

/// The manifest constants build.rs baked in.
mod baked {
    include!(concat!(env!("OUT_DIR"), "/baked.rs"));
}

/// The staged payload, ready to be attached to the machine.
enum Payload {
    /// Disc in the CD drive at power-on, served from the read-only sidecar;
    /// saves persist through the CD32's NVRAM in the per-game directory.
    Cd(PathBuf),
    /// Disk in DF0 at power-on: a per-user copy, so guest writes persist
    /// without touching the (possibly read-only) bundle.
    Adf(PathBuf),
    /// A plain executable booted `--run` style from a per-user copy of the
    /// game files tree.
    Run(runprog::PreparedRun),
}

fn main() -> Result<()> {
    // Order matters, and these two come before everything: sealing kills
    // every COPPERLINE_* knob before any code can snapshot the environment,
    // and the identity must be adopted before anything resolves the config
    // directory.
    envcfg::seal();
    paths::set_app_identity(baked::GAME_ID);
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = parse_args()?;
    if cli.version {
        println!(
            "{} {} (copperline-player {})",
            baked::GAME_TITLE,
            baked::GAME_VERSION,
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    let payload = stage_payload()?;

    // The machine, from the baked manifest: the profile's defaults, the
    // manifest's RAM and display choices, then whatever the user changed in
    // the menu last time (settings.toml), then this launch's own flags.
    let mut raw = RawConfig::default();
    raw.set_machine_profile(baked::MODEL);
    raw.set_memory_overrides(baked::CHIP_RAM, baked::FAST_RAM, baked::SLOW_RAM);
    raw.set_display_defaults(baked::SHADER, baked::BEZEL, baked::FULLSCREEN);
    raw.set_status_bar(false);
    if let Some(settings) = load_settings() {
        raw.merge_player_settings(&settings);
    }
    if let Some(fullscreen) = cli.fullscreen {
        raw.set_fullscreen(fullscreen);
    }
    let mut run_warp = None;
    match &payload {
        Payload::Cd(image) => raw.set_cd_image(image),
        Payload::Adf(image) => raw.set_boot_floppy(image),
        Payload::Run(prepared) => {
            runprog::apply_to_raw(&mut raw, prepared);
            run_warp = Some(runprog::WarpLaunch::new(
                prepared.prog_name.clone(),
                Some(prepared.boot_dir.join(runprog::DONE_MARKER)),
            ));
        }
    }

    // Adopt the (default) output paths before the conversion, which sites
    // the battery-RAM files -- CD32 NVRAM saves land per game through the
    // identity adopted above.
    paths::adopt(raw.paths());
    let raw_for_app = raw.clone();
    let mut cfg = Config::try_from(raw)?;
    config::resolve_bundled_rom(&mut cfg)?;

    // Headless verification (--screenshot-after): unthrottled, windowless,
    // silent -- how a publisher smoke-tests the assembled bundle in CI.
    let capture = !cli.screenshot_after.is_empty();
    let live_audio = cfg.audio.output_enabled && !capture;
    let audio: Box<dyn AudioSink> = if live_audio {
        Box::new(CpalSink::new(false, cfg.audio.output_device.as_deref())?)
    } else {
        Box::new(NullSink)
    };
    let emu = emulator::build_machine(&cfg, audio, !capture, false)?;

    video::set_pixel_aspect(config::resolve_pixel_aspect(cfg.pixel_aspect));
    video::set_display_scaling(cfg.scaling);
    video::set_menu_scale(cfg.menu_scale);
    video::set_player_profile(baked::SAVE_STATES);
    video::set_branding(
        baked::GAME_TITLE.to_string(),
        baked::ICON_PNG.map(<[u8]>::to_vec),
    );

    let disk_write_protected = std::array::from_fn(|idx| {
        cfg.floppy.drives[idx]
            .as_ref()
            .map(|d| d.write_protected)
            .unwrap_or(true)
    });
    // A capture run is unpaced end to end, so the warp-launch gate would
    // only re-pace it; interactive runs warp-boot to the program's load.
    let run_warp_target = if capture { None } else { run_warp };
    let app = App::new(
        emu,
        cfg.emulation.power_on,
        cli.screenshot_after,
        Vec::new(),
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        run_warp_target,
        cfg.floppy_playlists.clone(),
        disk_write_protected,
        config::resolve_overscan(cfg.overscan),
        cfg.tv_centre,
        config::resolve_deinterlace(cfg.deinterlace),
        config::resolve_phosphor(cfg.phosphor),
        config::resolve_shader(cfg.shader.clone()),
        config::resolve_shader_strength(cfg.shader_strength),
        config::resolve_bezel(cfg.bezel),
        None,
        false,
        config::resolve_tint(cfg.tint),
        cfg.full_screen,
        true,
        cfg.emulation.warp_speed,
        cfg.joystick_input_mode,
        cfg.mouse_sensitivity,
        cfg.mouse_capture,
        config::about_machine_lines(&cfg),
        raw_for_app,
        live_audio,
        copperline::sampler::SamplerRequest::default(),
    );
    if capture {
        log::info!("bundle verification: running without a window");
        return app.run_headless();
    }
    app.run()
}

struct CliArgs {
    /// `--fullscreen` / `--windowed`, overriding the saved setting.
    fullscreen: Option<bool>,
    version: bool,
    /// Undocumented bundle-verification flag, the headless smoke test a
    /// publisher's CI runs against the assembled bundle.
    screenshot_after: Vec<(f32, PathBuf)>,
}

fn parse_args() -> Result<CliArgs> {
    let mut cli = CliArgs {
        fullscreen: None,
        version: false,
        screenshot_after: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fullscreen" => cli.fullscreen = Some(true),
            "--windowed" => cli.fullscreen = Some(false),
            "--version" => cli.version = true,
            "--screenshot-after" => {
                let secs: f32 = args
                    .next()
                    .ok_or_else(|| anyhow!("--screenshot-after needs SECS PATH"))?
                    .parse()
                    .context("--screenshot-after SECS")?;
                let path = args
                    .next()
                    .ok_or_else(|| anyhow!("--screenshot-after needs SECS PATH"))?;
                cli.screenshot_after.push((secs, PathBuf::from(path)));
            }
            "--help" | "-h" => {
                println!(
                    "{}\n\nUsage: options are\n  \
                     --fullscreen    start fullscreen\n  \
                     --windowed      start in a window\n  \
                     --version       print the version and exit",
                    baked::GAME_TITLE
                );
                std::process::exit(0);
            }
            other => bail!("unknown option {other:?} (try --help)"),
        }
    }
    Ok(cli)
}

/// The per-user settings the in-game menu wrote last session, if any.
fn load_settings() -> Option<RawConfig> {
    let path = paths::config_file(config::PLAYER_SETTINGS_FILE)?;
    let text = std::fs::read_to_string(&path).ok()?;
    match RawConfig::parse(&text) {
        Ok(raw) => Some(raw),
        Err(e) => {
            log::warn!("ignoring unreadable {}: {e:#}", path.display());
            None
        }
    }
}

/// Find the sidecar payload and make it ready: verify the pin, and for the
/// writable kinds stage a per-user copy guarded by a fingerprint marker.
fn stage_payload() -> Result<Payload> {
    let sidecar = find_sidecar(baked::PAYLOAD_FILE).ok_or_else(|| {
        anyhow!(
            "the game data ({}) is missing from the {} bundle; reinstall the game",
            baked::PAYLOAD_FILE,
            baked::GAME_TITLE
        )
    })?;
    if let Some(pin) = baked::PAYLOAD_SHA256 {
        let digest = sha256_of_file(&sidecar)?;
        if digest != pin {
            bail!(
                "the game data ({}) does not match this build of {} \
                 (SHA-256 {digest}, expected {pin}); reinstall the game",
                sidecar.display(),
                baked::GAME_TITLE
            );
        }
    }
    match baked::PAYLOAD_KIND {
        "cd" => Ok(Payload::Cd(sidecar)),
        "adf" => {
            let copy = staged_copy(&sidecar)?;
            Ok(Payload::Adf(copy))
        }
        "run" => {
            let dir = staged_copy(&sidecar)?;
            let program = dir.join(baked::RUN_EXECUTABLE);
            if !program.is_file() {
                bail!(
                    "the game files do not contain the executable {:?}; reinstall the game",
                    baked::RUN_EXECUTABLE
                );
            }
            let args = Some(baked::RUN_ARGS).filter(|a| !a.is_empty());
            let prepared = runprog::prepare(&program, args, None)?;
            Ok(Payload::Run(prepared))
        }
        other => unreachable!("build.rs bakes only known payload kinds, not {other:?}"),
    }
}

/// Where the sidecar payload is looked for: beside the executable, in the
/// macOS bundle's Resources, and -- for development -- the working
/// directory. The same ladder the bundled AROS search walks.
fn find_sidecar(name: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("..").join("Resources"));
        }
    }
    dirs.push(PathBuf::from("."));
    dirs.into_iter().map(|d| d.join(name)).find(|p| p.exists())
}

/// The sidecar's SHA-256, streamed a megabyte at a time: a pinned payload
/// can be a CD image of hundreds of megabytes, which must never be held
/// in memory (let alone twice) just to be checked.
fn sha256_of_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file =
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = copperline::hash::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// The per-user copy of a writable payload (`adf` file or `run` tree),
/// staged WHDLoad-style under the per-game config directory.
///
/// A `.payload` marker records the sidecar's identity: the game version
/// and pin from the manifest, then every member's path, size, and mtime.
/// Unchanged, the copy is reused as it stands -- which is where the
/// guest's writes live, so reuse is what makes saves persist. Changed
/// (the publisher shipped an update), members the new payload no longer
/// carries are removed first, then the payload's own members are copied
/// over the old ones. Anything the marker never owned, which is exactly
/// the files the game created, is left alone, so saves survive updates.
fn staged_copy(sidecar: &Path) -> Result<PathBuf> {
    let home = paths::config_dir().context(
        "no per-user directory available to stage the game files (no HOME/APPDATA); \
         create a portable.txt beside the game to keep its data there instead",
    )?;
    let dest = home.join("game");
    let marker = home.join(".payload");
    let print = marker_contents(sidecar)?;
    let recorded = std::fs::read_to_string(&marker).ok();
    let dest_path = dest.join(baked::PAYLOAD_FILE);
    if recorded.as_deref() == Some(print.as_str()) && dest_path.exists() {
        log::info!(
            "game files: reusing {} (saves persist there)",
            dest.display()
        );
        return Ok(dest_path);
    }
    // An update to a run tree: members of the previous payload the new one
    // no longer carries are removed before the copy, so a renamed or
    // deleted file cannot linger and still be loaded. Everything the
    // marker never owned -- the files the game created -- is left alone.
    if sidecar.is_dir() {
        if let Some(old) = &recorded {
            for stale in marker_members(old).difference(&marker_members(&print)) {
                let _ = std::fs::remove_file(dest_path.join(stale));
            }
        }
    }
    std::fs::create_dir_all(&dest)?;
    copy_over(sidecar, &dest_path)?;
    std::fs::write(&marker, &print)?;
    log::info!("game files: staged into {}", dest.display());
    Ok(dest_path)
}

/// What the `.payload` marker holds: `#`-prefixed meta lines carrying the
/// baked game version and pin, then the sidecar's member fingerprint. The
/// meta lines are what catches an update that preserves every size and
/// timestamp (reproducible archives do): the publisher bumps the version,
/// or the pin -- content-exact by definition -- changes with the file.
fn marker_contents(sidecar: &Path) -> Result<String> {
    Ok(format!(
        "#version\t{}\n#pin\t{}\n{}",
        baked::GAME_VERSION,
        baked::PAYLOAD_SHA256.unwrap_or("-"),
        fingerprint(sidecar)?
    ))
}

/// The member paths a marker records: everything the payload owns in the
/// staged tree, and nothing the guest wrote. Entries that are not plain
/// relative paths are dropped rather than resolved -- the marker is the
/// player's own file, but a removal must still never reach outside the
/// staged copy.
fn marker_members(marker: &str) -> std::collections::BTreeSet<PathBuf> {
    marker
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split('\t').next())
        .map(PathBuf::from)
        .filter(|path| {
            path.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        })
        .collect()
}

/// Copy the sidecar over the staged copy: a file plainly, a tree
/// member-by-member (existing extra files are left alone).
fn copy_over(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_over(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to)
            .map(|_| ())
            .with_context(|| format!("copying {} to {}", from.display(), to.display()))
    }
}

/// A cheap change detector for the sidecar: every member's relative path,
/// size, and mtime -- never the content, which would cost a full read of
/// the payload on every launch. An update that preserves both size and
/// timestamp is caught by the marker's version/pin meta lines instead
/// ([`marker_contents`]); a fresh install's mtimes count as a change,
/// which just costs one harmless recopy.
fn fingerprint(path: &Path) -> Result<String> {
    fn entry(out: &mut String, root: &Path, path: &Path) -> Result<()> {
        let meta = std::fs::metadata(path)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let rel = path.strip_prefix(root).unwrap_or(path);
        out.push_str(&format!("{}\t{}\t{}\n", rel.display(), meta.len(), mtime));
        Ok(())
    }
    let mut out = String::new();
    if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    entry(&mut out, path, &p)?;
                }
            }
        }
    } else {
        entry(&mut out, path.parent().unwrap_or(Path::new("")), path)?;
    }
    Ok(out)
}
