// SPDX-License-Identifier: GPL-3.0-or-later

//! Copperline: Amiga emulator.
//!
//! Usage: copperline [--config FILE] [ROM]
//!   If no --config is given, looks for ./copperline.toml.
//!   If no ROM is given (neither argument nor `rom =` in the config), boots
//!   the bundled AROS open-source Kickstart replacement (see src/romsearch.rs).

use anyhow::{anyhow, Result};
use copperline::{config, debugger, emulator, envcfg, gamepad, gdbstub, priority, video};
use log::{info, warn};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use copperline::audio::{AudioSink, CpalSink, NullSink, WavSink};
use copperline::bus::Bus;
use copperline::chipset::paula::{Paula, DMACON_DMAEN, PAULA_CLOCK_HZ};
use copperline::config::{Chipset, Config, ConfigOverrides};
use copperline::emulator::Emulator;
use copperline::floppy::FloppyController;
use copperline::memory::Memory;
use copperline::serial::StdoutSink;
use copperline::video::window::{
    parse_amiga_key, App, DiskInsertSpec, FrameDumpSpec, KeyPressSpec, DEFAULT_KEY_HOLD_MS,
};
use copperline::video::HOST_SHORTCUT_MODIFIER_LABEL;

#[derive(Debug)]
pub struct CliArgs {
    pub config_path: Option<PathBuf>,
    pub rom_path: Option<PathBuf>,
    pub screenshot_after: Option<(f32, PathBuf)>,
    /// `--save-state-after SECS PATH`: write a save state of the whole
    /// machine after SECS emulated seconds, then keep running (combine
    /// with --screenshot-after/--dump-frames to bound the run).
    pub save_state_after: Option<(f32, PathBuf)>,
    /// `--load-state PATH`: restore a save state before entering the
    /// event loop, resuming from its emulated timeline.
    pub load_state: Option<PathBuf>,
    /// `--benchmark-until SECS`: run frames directly, without opening a
    /// window, until the absolute emulated-time target is reached.
    pub benchmark_until: Option<f32>,
    /// `--gdb ADDR`: run a headless GDB remote-protocol server on ADDR,
    /// `:PORT`, or `PORT`, pausing at reset until the debugger resumes.
    pub gdb: Option<gdbstub::Config>,
    /// Dump consecutive rendered frames after an emulated-time delay. This
    /// is intended for debugging flicker and frame-to-frame palette
    /// changes that a single screenshot cannot show.
    pub frame_dump: Option<FrameDumpSpec>,
    /// Scripted key presses to inject after the window opens. Useful
    /// for headless testing of menus and modifier chords.
    pub press_after: Vec<KeyPressSpec>,
    /// `--click-after SECS BUTTON DURATION_MS`: at SECS seconds after
    /// the window opens, press the named mouse button (left/right/middle),
    /// hold for DURATION_MS, then release. Useful for headless testing
    /// of the mouse-button-driven wait prompts.
    pub click_after: Vec<(f32, MouseButtonKind, u32)>,
    /// `--joy-after SECS BUTTON DURATION_MS`: at SECS emulated seconds,
    /// press a port-2 joystick / CD32-pad control (up/down/left/right/
    /// red|fire/blue/green/yellow/play/rwd/ffw), hold for DURATION_MS,
    /// then release. Useful for headless testing of joystick-driven
    /// titles, especially CD32 games whose pad otherwise needs a
    /// calibrated physical gamepad.
    pub joy_after: Vec<(f32, JoyButtonKind, u32)>,
    /// `--mouse-after SECS DX DY`: at SECS emulated seconds, apply a
    /// relative port-1 mouse motion of (DX, DY) counter steps. Emitted
    /// by the input recorder one event per frame of recorded movement.
    pub mouse_after: Vec<(f32, i32, i32)>,
    /// `--record-input PATH`: record every input event that reaches the
    /// emulated machine for the whole run and write the scripted-input
    /// file to PATH on exit (the windowed toggle is the host shortcut
    /// modifier plus Shift+R).
    pub record_input: Option<PathBuf>,
    /// Scripted floppy image insertion. This supports both explicit
    /// paths and deferring a disk image already configured in the TOML.
    pub disk_insert_after: Vec<CliDiskInsert>,
    /// Real-time stereo audio output through cpal. Enabled by default;
    /// `--noaudio` disables it, and `--audio-wav` selects WAV output.
    pub audio_live: bool,
    /// `--audio-wav PATH`: dump the mixed stereo output to a WAV file
    /// (32-bit float, 44100 Hz). No live output. Useful for headless
    /// verification of the audio path.
    pub audio_wav: Option<PathBuf>,
    /// `--profile-live-audio SECS`: run a no-window Paula-to-cpal
    /// profile workload for SECS seconds. Use COPPERLINE_AUDIO_PROFILE=1
    /// to emit the live-audio counters while it runs.
    pub live_audio_profile_secs: Option<f32>,
    /// `--calibrate-gamepad`: run the interactive gamepad calibration and
    /// exit, without starting the emulator.
    pub calibrate_gamepad: bool,
    /// `--list-midi`: print the host MIDI endpoints and exit.
    pub list_midi: bool,
    /// Command-line machine overrides (`--model`, `--chipset`, `--cpu`,
    /// `--fpu`/`--no-fpu`, `--cpu-clock`, `--chip`, `--fast`, `--slow`,
    /// `--floppy-drives`).
    /// Applied on top of the config file (or the built-in defaults) before
    /// validation.
    pub overrides: ConfigOverrides,
}

use copperline::video::window::{JoyButtonKind, MouseButtonKind};

#[derive(Debug, Clone, PartialEq)]
pub enum CliDiskInsert {
    Explicit(DiskInsertSpec),
    Configured { secs: f32, drive_idx: usize },
}

fn parse_args() -> Result<CliArgs> {
    parse_args_from(std::env::args().skip(1))
}

/// Scripted-input directives accepted inside a `--script` file. These are
/// the flag names (without the leading dashes) whose effects accumulate;
/// anything else in a script is an error so a typo cannot silently change
/// emulator configuration.
const SCRIPT_DIRECTIVES: [&str; 8] = [
    "press-after",
    "key-after",
    "hold-key-after",
    "click-after",
    "joy-after",
    "mouse-after",
    "insert-disk-after",
    "defer-disk-insert",
];

/// Split one script line into tokens: whitespace-separated, with
/// double-quoted tokens allowed to carry spaces (for disk-image paths).
fn tokenize_script_line(line: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' {
            chars.next();
            let mut tok = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => tok.push(c),
                    None => return Err(anyhow!("unterminated quote in script line {line:?}")),
                }
            }
            tokens.push(tok);
        } else {
            let mut tok = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                tok.push(c);
                chars.next();
            }
            tokens.push(tok);
        }
    }
    Ok(tokens)
}

/// Expand every `--script FILE` argument in place: each non-empty,
/// non-`#` line of the file is a scripted-input directive in the flag
/// syntax without the leading dashes (`key-after 14.0 ctrl 500`), and
/// expands to the equivalent flags for the main parser. Scripts cannot
/// include other scripts.
fn expand_script_files(args: Vec<String>) -> Result<Vec<String>> {
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg != "--script" {
            out.push(arg);
            continue;
        }
        let path = iter
            .next()
            .ok_or_else(|| anyhow!("--script requires a path"))?;
        let text =
            std::fs::read_to_string(&path).map_err(|e| anyhow!("reading script {path}: {e}"))?;
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let tokens = tokenize_script_line(line)?;
            let Some((directive, rest)) = tokens.split_first() else {
                continue;
            };
            if !SCRIPT_DIRECTIVES.contains(&directive.as_str()) {
                return Err(anyhow!(
                    "{path}:{}: {directive:?} is not a scripted-input directive \
                     (allowed: {})",
                    lineno + 1,
                    SCRIPT_DIRECTIVES.join(", ")
                ));
            }
            out.push(format!("--{directive}"));
            out.extend(rest.iter().cloned());
        }
    }
    Ok(out)
}

/// Parse the next CLI argument as `T`: the common shape behind most of
/// this parser's `--flag VALUE` options, which otherwise each repeat the
/// same "missing argument" / "not a valid value" error-handling pair.
/// `missing` names what the whole flag needs when no argument follows;
/// `invalid` explains what shape this particular value must have.
fn next_arg<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    missing: &str,
    invalid: &str,
) -> Result<T> {
    args.next()
        .ok_or_else(|| anyhow!("{missing}"))?
        .parse()
        .map_err(|_| anyhow!("{invalid}"))
}

fn parse_args_from<I>(args: I) -> Result<CliArgs>
where
    I: IntoIterator<Item = String>,
{
    let args = expand_script_files(args.into_iter().collect())?;
    let mut config_path: Option<PathBuf> = None;
    let mut rom_path: Option<PathBuf> = None;
    let mut screenshot_after: Option<(f32, PathBuf)> = None;
    let mut save_state_after: Option<(f32, PathBuf)> = None;
    let mut load_state: Option<PathBuf> = None;
    let mut benchmark_until: Option<f32> = None;
    let mut gdb: Option<gdbstub::Config> = None;
    let mut dump_dir: Option<PathBuf> = None;
    let mut dump_start_secs: f32 = 0.0;
    let mut dump_count: Option<u32> = None;
    let mut press_after: Vec<KeyPressSpec> = Vec::new();
    let mut click_after: Vec<(f32, MouseButtonKind, u32)> = Vec::new();
    let mut joy_after: Vec<(f32, JoyButtonKind, u32)> = Vec::new();
    let mut mouse_after: Vec<(f32, i32, i32)> = Vec::new();
    let mut record_input: Option<PathBuf> = None;
    let mut disk_insert_after: Vec<CliDiskInsert> = Vec::new();
    let mut audio_live = true;
    let mut explicit_audio_live = false;
    let mut explicit_noaudio = false;
    let mut audio_wav: Option<PathBuf> = None;
    let mut live_audio_profile_secs: Option<f32> = None;
    let mut calibrate_gamepad = false;
    let mut list_midi = false;
    let mut overrides = ConfigOverrides::default();
    let mut args = args.into_iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--calibrate-gamepad" => {
                calibrate_gamepad = true;
            }
            "--list-midi" => {
                list_midi = true;
            }
            "--config" | "-c" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--config requires a path"))?;
                config_path = Some(PathBuf::from(v));
            }
            "--model" => {
                overrides.model = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--model requires a name (A500/A600/A1200/...)"))?,
                );
            }
            "--chipset" => {
                overrides.chipset = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--chipset requires OCS/ECS/AGA"))?,
                );
            }
            "--cpu" => {
                overrides.cpu = Some(args.next().ok_or_else(|| {
                    anyhow!("--cpu requires a model (68000/68EC020/68020/68030/68040/68060)")
                })?);
            }
            "--fpu" => {
                overrides.fpu = Some(true);
            }
            "--no-fpu" => {
                overrides.fpu = Some(false);
            }
            "--cpu-clock" => {
                let mhz: f64 = next_arg(
                    &mut args,
                    "--cpu-clock requires MHZ",
                    "--cpu-clock MHZ must be a number",
                )?;
                overrides.cpu_clock_mhz = Some(mhz);
            }
            "--chip" => {
                overrides.chip = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--chip requires a size (e.g. 512K, 1M, 2M)"))?,
                );
            }
            "--fast" => {
                overrides.fast = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--fast requires a size (e.g. 0, 4M, 8M)"))?,
                );
            }
            "--slow" => {
                overrides.slow = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--slow requires a size (e.g. 0, 512K)"))?,
                );
            }
            "--floppy-drives" | "--fdd-drives" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--floppy-drives requires COUNT (1-4)"))?;
                overrides.floppy_drives = Some(parse_floppy_drive_count(&value)?);
            }
            "--joystick" => {
                overrides.joystick = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--joystick requires a mode (gamepad/keyboard)"))?,
                );
            }
            "--serial" => {
                overrides.serial = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--serial requires a mode (off/stdout/midi)"))?,
                );
            }
            "--midi-out" => {
                overrides.midi_out = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--midi-out requires a device name"))?,
                );
            }
            "--midi-in" => {
                overrides.midi_in = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--midi-in requires a device name"))?,
                );
            }
            "--click-after" => {
                const USAGE: &str = "--click-after requires SECS BUTTON DURATION_MS";
                let secs: f32 = next_arg(&mut args, USAGE, "--click-after SECS must be a number")?;
                let button_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let button = match button_s.as_str() {
                    "left" | "lmb" | "l" => MouseButtonKind::Left,
                    "right" | "rmb" | "r" => MouseButtonKind::Right,
                    "middle" | "mmb" | "m" => MouseButtonKind::Middle,
                    _ => return Err(anyhow!("--click-after BUTTON must be left/right/middle")),
                };
                let dur_ms: u32 = next_arg(
                    &mut args,
                    USAGE,
                    "--click-after DURATION_MS must be a number",
                )?;
                click_after.push((secs, button, dur_ms));
            }
            "--joy-after" => {
                const USAGE: &str = "--joy-after requires SECS BUTTON DURATION_MS";
                let secs: f32 = next_arg(&mut args, USAGE, "--joy-after SECS must be a number")?;
                let button_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let button = JoyButtonKind::parse(&button_s).ok_or_else(|| {
                    anyhow!(
                        "--joy-after BUTTON must be up/down/left/right/red/blue/green/yellow/play/rwd/ffw"
                    )
                })?;
                let dur_ms: u32 =
                    next_arg(&mut args, USAGE, "--joy-after DURATION_MS must be a number")?;
                joy_after.push((secs, button, dur_ms));
            }
            "--mouse-after" => {
                const USAGE: &str = "--mouse-after requires SECS DX DY";
                let secs: f32 = next_arg(&mut args, USAGE, "--mouse-after SECS must be a number")?;
                let dx: i32 = next_arg(&mut args, USAGE, "--mouse-after DX must be an integer")?;
                let dy: i32 = next_arg(&mut args, USAGE, "--mouse-after DY must be an integer")?;
                mouse_after.push((secs, dx, dy));
            }
            "--record-input" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--record-input requires a path"))?;
                record_input = Some(PathBuf::from(v));
            }
            "--insert-disk-after" => {
                const USAGE: &str = "--insert-disk-after requires SECS DFN PATH";
                let secs: f32 = next_arg(
                    &mut args,
                    USAGE,
                    "--insert-disk-after SECS must be a number",
                )?;
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let drive_idx = parse_floppy_drive_idx(&drive_s, "--insert-disk-after")?;
                let path = args.next().ok_or_else(|| anyhow!(USAGE))?;
                disk_insert_after.push(CliDiskInsert::Explicit(DiskInsertSpec {
                    secs,
                    drive_idx,
                    path: PathBuf::from(path),
                    write_protected: true,
                }));
            }
            "--defer-disk-insert" => {
                const USAGE: &str = "--defer-disk-insert requires SECS DFN";
                let secs: f32 = next_arg(
                    &mut args,
                    USAGE,
                    "--defer-disk-insert SECS must be a number",
                )?;
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let drive_idx = parse_floppy_drive_idx(&drive_s, "--defer-disk-insert")?;
                disk_insert_after.push(CliDiskInsert::Configured { secs, drive_idx });
            }
            "--press-after" => {
                const USAGE: &str = "--press-after requires SECS KEY";
                let secs: f32 = next_arg(&mut args, USAGE, "--press-after SECS must be a number")?;
                let key_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let rawkey = parse_amiga_key(&key_s)
                    .ok_or_else(|| anyhow!("--press-after KEY: unknown key {:?}", key_s))?;
                press_after.push(KeyPressSpec {
                    secs,
                    rawkey,
                    hold_ms: DEFAULT_KEY_HOLD_MS,
                });
            }
            "--key-after" | "--hold-key-after" => {
                const USAGE: &str = "--key-after requires SECS KEY DURATION_MS";
                let secs: f32 = next_arg(&mut args, USAGE, "--key-after SECS must be a number")?;
                let key_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let rawkey = parse_amiga_key(&key_s)
                    .ok_or_else(|| anyhow!("--key-after KEY: unknown key {:?}", key_s))?;
                let hold_ms: u32 =
                    next_arg(&mut args, USAGE, "--key-after DURATION_MS must be a number")?;
                press_after.push(KeyPressSpec {
                    secs,
                    rawkey,
                    hold_ms,
                });
            }
            "--screenshot-after" => {
                const USAGE: &str = "--screenshot-after requires SECS PATH";
                let secs: f32 =
                    next_arg(&mut args, USAGE, "--screenshot-after SECS must be a number")?;
                let path = args.next().ok_or_else(|| anyhow!(USAGE))?;
                screenshot_after = Some((secs, PathBuf::from(path)));
            }
            "--save-state-after" => {
                const USAGE: &str = "--save-state-after requires SECS PATH";
                let secs: f32 =
                    next_arg(&mut args, USAGE, "--save-state-after SECS must be a number")?;
                let path = args.next().ok_or_else(|| anyhow!(USAGE))?;
                save_state_after = Some((secs, PathBuf::from(path)));
            }
            "--load-state" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--load-state requires a path"))?;
                load_state = Some(PathBuf::from(v));
            }
            "--benchmark-until" | "--bench-until" => {
                let secs: f32 = next_arg(
                    &mut args,
                    "--benchmark-until requires SECS",
                    "--benchmark-until SECS must be a number",
                )?;
                if secs <= 0.0 {
                    return Err(anyhow!("--benchmark-until SECS must be greater than zero"));
                }
                benchmark_until = Some(secs);
            }
            "--gdb" | "--gdb-listen" => {
                let listen = args
                    .next()
                    .ok_or_else(|| anyhow!("--gdb requires ADDR, :PORT, or PORT"))?;
                gdb = Some(gdbstub::Config::new(listen));
            }
            "--dump-frames" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow!("--dump-frames requires a directory"))?;
                dump_dir = Some(PathBuf::from(path));
            }
            "--dump-start" => {
                dump_start_secs = next_arg(
                    &mut args,
                    "--dump-start requires SECS",
                    "--dump-start SECS must be a number",
                )?;
            }
            "--dump-count" => {
                let count: u32 = next_arg(
                    &mut args,
                    "--dump-count requires COUNT",
                    "--dump-count COUNT must be a positive integer",
                )?;
                if count == 0 {
                    return Err(anyhow!("--dump-count COUNT must be greater than zero"));
                }
                dump_count = Some(count);
            }
            "--audio" => {
                audio_live = true;
                explicit_audio_live = true;
            }
            "--noaudio" | "--no-audio" => {
                audio_live = false;
                explicit_noaudio = true;
            }
            "--audio-wav" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--audio-wav requires a path"))?;
                audio_wav = Some(PathBuf::from(v));
                audio_live = false;
            }
            "--profile-live-audio" => {
                let secs: f32 = next_arg(
                    &mut args,
                    "--profile-live-audio requires SECS",
                    "--profile-live-audio SECS must be a number",
                )?;
                if secs <= 0.0 {
                    return Err(anyhow!(
                        "--profile-live-audio SECS must be greater than zero"
                    ));
                }
                live_audio_profile_secs = Some(secs);
                audio_live = true;
                explicit_audio_live = true;
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("copperline {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other if other.starts_with("--") => {
                return Err(anyhow!("unknown option {:?} (see --help)", other));
            }
            _ => {
                if rom_path.is_some() {
                    return Err(anyhow!("more than one ROM path given"));
                }
                rom_path = Some(PathBuf::from(a));
            }
        }
    }
    if explicit_audio_live && audio_wav.is_some() {
        return Err(anyhow!("--audio and --audio-wav are mutually exclusive"));
    }
    if live_audio_profile_secs.is_some() && explicit_noaudio {
        return Err(anyhow!(
            "--profile-live-audio and --noaudio are mutually exclusive"
        ));
    }
    if (benchmark_until.is_some() || gdb.is_some()) && !explicit_audio_live && audio_wav.is_none() {
        audio_live = false;
    }
    let frame_dump = match (dump_dir, dump_count) {
        (Some(dir), Some(count)) => Some(FrameDumpSpec {
            dir,
            start_secs: dump_start_secs,
            count,
        }),
        (Some(_), None) => return Err(anyhow!("--dump-frames requires --dump-count COUNT")),
        (None, Some(_)) => return Err(anyhow!("--dump-count requires --dump-frames DIR")),
        (None, None) => {
            if dump_start_secs != 0.0 {
                return Err(anyhow!("--dump-start requires --dump-frames DIR"));
            }
            None
        }
    };
    Ok(CliArgs {
        config_path,
        rom_path,
        screenshot_after,
        save_state_after,
        load_state,
        benchmark_until,
        gdb,
        frame_dump,
        press_after,
        click_after,
        joy_after,
        mouse_after,
        record_input,
        disk_insert_after,
        audio_live,
        audio_wav,
        live_audio_profile_secs,
        calibrate_gamepad,
        list_midi,
        overrides,
    })
}

fn print_help() {
    let shortcut = HOST_SHORTCUT_MODIFIER_LABEL;
    // The MIDI endpoint options only do anything in a `midi`-feature build, so
    // list them only there. `--serial` itself is always shown: off/stdout work
    // in every build, and it names midi as a mode.
    #[cfg(feature = "midi")]
    let midi = "--midi-out NAME                host MIDI destination (implies --serial midi)\n  \
                --midi-in NAME                 host MIDI source (implies --serial midi)\n  \
                --list-midi                    list host MIDI endpoints and exit\n  ";
    #[cfg(not(feature = "midi"))]
    let midi = "";
    eprintln!(
        "copperline - Amiga emulator\n\
         \n\
         Usage: copperline [--config FILE] [--screenshot-after SECS PATH] [ROM]\n\
         \n\
         Options:\n  \
         -c, --config FILE              load configuration from FILE (default: ./copperline.toml)\n  \
         --model NAME                   machine profile: A500, A500Plus, A600, A1200, CDTV, CD32\n  \
         --chipset NAME                 chipset preset: OCS, ECS, or AGA\n  \
         --cpu MODEL                    CPU: 68000, 68EC020, 68020, 68030, 68040, or 68060\n  \
         --cpu-clock MHZ                CPU clock in MHz (default: the model's stock speed)\n  \
         --fpu / --no-fpu               fit / omit a 68881/68882 (68040/68060 on-die)\n  \
         --chip SIZE                    chip RAM size, e.g. 512K, 1M, 2M\n  \
         --fast SIZE                    Zorro II fast RAM size, e.g. 0, 1M, 4M, 8M\n  \
         --slow SIZE                    trapdoor slow RAM at $C00000, e.g. 0, 512K\n  \
         --floppy-drives COUNT          wired floppy drives, 1-4 (DF0 plus externals)\n  \
         --joystick MODE                initial joystick input: gamepad or keyboard\n  \
         \x20                            (gamepad lets the keyboard pass through to the Amiga)\n  \
         \x20                            (--model/--cpu/etc. override the config file or defaults)\n  \
         --screenshot-after SECS PATH   save a PNG to PATH after SECS emulated seconds, then exit\n  \
         --save-state-after SECS PATH   write a save state to PATH after SECS emulated seconds,\n  \
         \x20                            then keep running\n  \
         --load-state PATH              restore a save state before starting, resuming from\n  \
         \x20                            its emulated timeline\n  \
         --benchmark-until SECS         run frames with no window until absolute emulated\n  \
         \x20                            time SECS, report counters, then exit\n  \
         --gdb ADDR                     run a headless GDB remote server on ADDR,\n  \
         \x20                            :PORT, or PORT; port-only forms bind 127.0.0.1\n  \
         --dump-frames DIR              dump consecutive PNG frames into DIR, then exit\n  \
         --dump-start SECS              start frame dumping after SECS seconds (default: 0)\n  \
         --dump-count COUNT             number of frames to dump with --dump-frames\n  \
         --press-after SECS KEY         press/release Amiga KEY after SECS; KEY may be\n  \
         \x20                            decimal, 0x.., or a name like ctrl/lalt/lami/f1\n  \
         --key-after SECS KEY MS        press KEY after SECS, hold for MS milliseconds,\n  \
         \x20                            then release; may be passed multiple times\n  \
         --click-after SECS BTN MS      press mouse BTN (left/right/middle) at SECS,\n  \
         \x20                            release MS milliseconds later\n  \
         --mouse-after SECS DX DY       apply a relative port-1 mouse motion at SECS\n  \
         --record-input PATH            record all machine-bound input for the whole run\n  \
         \x20                            and write the script to PATH on exit\n  \
         --script FILE                  run scripted-input directives from FILE (the flag\n  \
         \x20                            syntax without the dashes; # comments allowed);\n  \
         \x20                            {shortcut}+Shift+R records a live session into this format\n  \
         --insert-disk-after SECS DFN PATH\n  \
         \x20                            insert PATH into DFN after SECS seconds\n  \
         --defer-disk-insert SECS DFN   start with configured DFN empty, then insert\n  \
         \x20                            its configured disk image after SECS seconds\n  \
         --audio                        enable real-time stereo audio output via cpal (default)\n  \
         --noaudio                      disable real-time audio output\n  \
         --audio-wav PATH               dump mixed stereo audio to a 32-bit float WAV file\n  \
         \x20                            instead of live output\n  \
         --profile-live-audio SECS      run a no-window Paula-to-cpal profile workload;\n  \
         \x20                            combine with COPPERLINE_AUDIO_PROFILE=1 for counters\n  \
         --serial MODE                  Paula serial port: off, stdout, or midi\n  \
         {midi}--calibrate-gamepad            interactively bind a USB gamepad to the port-2\n  \
         \x20                            joystick, save the calibration, then exit\n  \
         -h, --help                     show this help and exit\n  \
         -V, --version                  print the version and exit\n\
         \n\
         Window keys:\n  \
         {shortcut}+S save framebuffer to copperline-screenshot-<unix-ts>.png in cwd\n  \
         {shortcut}+D swap to the next disk in a drive's configured playlist\n  \
         {shortcut}+G capture/release host mouse; clicking the display also captures\n  \
         {shortcut}+Q quit\n\
         \n\
         Status bar: every connected floppy drive gets load (multi-select to\n\
         queue a swap playlist), swap, and eject buttons; CDTV/CD32 machines\n\
         add CD load and eject; plus screenshot, volume, pause, power, reboot.\n\
         \n\
         If ROM is given on the command line it overrides the rom path from\n\
         the config. If no config file exists, built-in defaults are used:\n  \
         CPU: 68000   chip RAM: 512K   slow RAM: 512K   fast RAM: 0   chipset: OCS\n  \
         ROM: bundled AROS"
    );
}

fn parse_floppy_drive_idx(s: &str, option: &str) -> Result<usize> {
    let drive = s.trim().to_ascii_lowercase();
    let drive = drive.strip_suffix(':').unwrap_or(&drive);
    let number = drive.strip_prefix("df").unwrap_or(drive);
    let idx: usize = number
        .parse()
        .map_err(|_| anyhow!("{option} drive must be df0, df1, df2, or df3"))?;
    if idx >= 4 {
        return Err(anyhow!("{option} drive must be df0, df1, df2, or df3"));
    }
    Ok(idx)
}

fn parse_floppy_drive_count(s: &str) -> Result<u8> {
    let count: u8 = s
        .parse()
        .map_err(|_| anyhow!("--floppy-drives COUNT must be an integer from 1 to 4"))?;
    if !(1..=4).contains(&count) {
        return Err(anyhow!(
            "--floppy-drives COUNT must be an integer from 1 to 4"
        ));
    }
    Ok(count)
}

fn resolve_disk_insert_after(
    cfg: &mut Config,
    disk_insert_after: Vec<CliDiskInsert>,
) -> Result<Vec<DiskInsertSpec>> {
    let mut out = Vec::new();
    for insert in disk_insert_after {
        match insert {
            CliDiskInsert::Explicit(spec) => {
                if !cfg.floppy_connected[spec.drive_idx] {
                    return Err(anyhow!(
                        "--insert-disk-after df{} needs a connected drive; \
                         use --floppy-drives {} or configure floppy.df{}",
                        spec.drive_idx,
                        spec.drive_idx + 1,
                        spec.drive_idx
                    ));
                }
                out.push(spec);
            }
            CliDiskInsert::Configured { secs, drive_idx } => {
                let Some(drive) = cfg.floppy.drives[drive_idx].take() else {
                    return Err(anyhow!(
                        "--defer-disk-insert df{} requires configured floppy.df{}",
                        drive_idx,
                        drive_idx
                    ));
                };
                out.push(DiskInsertSpec {
                    secs,
                    drive_idx,
                    path: drive.path,
                    write_protected: drive.write_protected,
                });
            }
        }
    }
    Ok(out)
}

fn validate_benchmark_args(cli: &CliArgs) -> Result<()> {
    if cli.benchmark_until.is_none() {
        return Ok(());
    }

    if cli.screenshot_after.is_some() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with --screenshot-after"
        ));
    }
    if cli.save_state_after.is_some() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with --save-state-after"
        ));
    }
    if cli.frame_dump.is_some() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with --dump-frames"
        ));
    }
    if cli.live_audio_profile_secs.is_some() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with --profile-live-audio"
        ));
    }
    if !cli.press_after.is_empty()
        || !cli.click_after.is_empty()
        || !cli.joy_after.is_empty()
        || !cli.mouse_after.is_empty()
    {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with scheduled input events"
        ));
    }
    if cli.record_input.is_some() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with --record-input"
        ));
    }
    if !cli.disk_insert_after.is_empty() {
        return Err(anyhow!(
            "--benchmark-until cannot be combined with scheduled disk inserts"
        ));
    }

    Ok(())
}

fn validate_gdb_args(cli: &CliArgs) -> Result<()> {
    if cli.gdb.is_none() {
        return Ok(());
    }

    if cli.benchmark_until.is_some() {
        return Err(anyhow!("--gdb cannot be combined with --benchmark-until"));
    }
    if cli.screenshot_after.is_some() {
        return Err(anyhow!("--gdb cannot be combined with --screenshot-after"));
    }
    if cli.save_state_after.is_some() {
        return Err(anyhow!("--gdb cannot be combined with --save-state-after"));
    }
    if cli.frame_dump.is_some() {
        return Err(anyhow!("--gdb cannot be combined with --dump-frames"));
    }
    if cli.live_audio_profile_secs.is_some() {
        return Err(anyhow!(
            "--gdb cannot be combined with --profile-live-audio"
        ));
    }
    if !cli.press_after.is_empty()
        || !cli.click_after.is_empty()
        || !cli.joy_after.is_empty()
        || !cli.mouse_after.is_empty()
    {
        return Err(anyhow!(
            "--gdb cannot be combined with scheduled input events"
        ));
    }
    if cli.record_input.is_some() {
        return Err(anyhow!("--gdb cannot be combined with --record-input"));
    }
    if !cli.disk_insert_after.is_empty() {
        return Err(anyhow!(
            "--gdb cannot be combined with scheduled disk inserts"
        ));
    }
    Ok(())
}

fn run_headless_benchmark(mut emu: Emulator, target_secs: f32) -> Result<()> {
    emu.set_paced(false);
    emu.reset_stats();

    let start_emulated = emu.bus().emulated_seconds();
    let target_secs = f64::from(target_secs);
    if target_secs <= start_emulated {
        return Err(anyhow!(
            "--benchmark-until target {:.3}s is not after current emulated time {:.3}s",
            target_secs,
            start_emulated
        ));
    }

    let start_frames = emu.bus().emulated_frames();
    let started = Instant::now();
    while emu.bus().emulated_seconds() < target_secs {
        emu.step_frame()?;
    }
    let elapsed = started.elapsed().as_secs_f64();
    let frames = emu.bus().emulated_frames().saturating_sub(start_frames);
    let emulated = emu.bus().emulated_seconds() - start_emulated;
    info!(
        "benchmark: ran {:.3}s emulated to {:.3}s target in {:.3}s wall, {} frames ({:.1}/s)",
        emulated,
        target_secs,
        elapsed,
        frames,
        frames as f64 / elapsed.max(f64::EPSILON)
    );
    emu.report_stats();
    // Evaluate an untargeted reverse watchpoint at the benchmark's end.
    emu.tt_finalize_reverse_watch()?;
    Ok(())
}

/// Print the host MIDI endpoints for `--list-midi`. This is how a user finds the
/// names `--midi-out`/`--midi-in` and `[serial]` expect. Without the `midi`
/// feature it says how to get MIDI support rather than printing nothing.
#[cfg(feature = "midi")]
fn list_midi_endpoints() -> Result<()> {
    let endpoints = copperline::midi::enumerate();
    println!("MIDI inputs (sources, for --midi-in):");
    if endpoints.inputs.is_empty() {
        println!("  (none)");
    }
    for e in &endpoints.inputs {
        println!("  {}", e.name);
    }
    println!("MIDI outputs (destinations, for --midi-out):");
    if endpoints.outputs.is_empty() {
        println!("  (none)");
    }
    for e in &endpoints.outputs {
        println!("  {}", e.name);
    }
    Ok(())
}

#[cfg(not(feature = "midi"))]
fn list_midi_endpoints() -> Result<()> {
    println!("This build has no MIDI support; rebuild with --features midi.");
    Ok(())
}

fn main() -> Result<()> {
    let mut log_builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    // Copperline reads raw gamepad axis/button codes with gilrs's SDL
    // controller mappings disabled (see gamepad.rs) and applies its own
    // per-UUID calibration from gamepads.toml. gilrs's mapping subsystem is
    // therefore unused, so its "No mapping found for UUID ...; default mapping
    // will be used" warnings are misleading noise even when the pad works.
    // Silence gilrs below error level unless the user has explicitly asked for
    // its logs via RUST_LOG.
    if std::env::var_os("RUST_LOG").is_none() {
        log_builder.filter_module("gilrs", log::LevelFilter::Error);
    }
    log_builder.init();

    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("\n!!! PANIC: {info}");
        prev(info);
    }));

    let cli = parse_args()?;
    validate_benchmark_args(&cli)?;
    validate_gdb_args(&cli)?;
    if cli.calibrate_gamepad {
        return gamepad::run_calibration();
    }
    if cli.list_midi {
        return list_midi_endpoints();
    }
    let (cfg, mut raw_cfg) = load_config(cli.config_path.as_deref(), &cli.overrides)?;
    if let Some(p) = &cli.rom_path {
        raw_cfg.rom = Some(p.to_string_lossy().into_owned());
    }

    // With nothing specified, open the configuration screen instead of booting
    // a default machine. Decided before resolving the bundled ROM so the
    // launcher opens even when no Kickstart/AROS is present.
    if launcher_requested(&cli) {
        return run_configuration_screen(raw_cfg);
    }

    let mut cfg = cfg.with_rom_override(cli.rom_path.clone());
    if cli.load_state.is_some() {
        // A save state restores the full ROM image, so a Kickstart file is not
        // required to load one. Still resolve the bundled-AROS sentinel when
        // AROS is installed (best effort, so the banner and any post-load reuse
        // see real paths); build_machine substitutes a placeholder for whatever
        // ROM is still unavailable.
        let _ = config::resolve_bundled_rom(&mut cfg);
    } else {
        config::resolve_bundled_rom(&mut cfg)?;
    }
    let disk_insert_after = resolve_disk_insert_after(&mut cfg, cli.disk_insert_after)?;

    info!(
        "config: cpu={:?} fpu={} cpu_clock={}MHz chip_ram={}K fast_ram={}K slow_ram={}K z3_ram={}K zorro_boards={} chipset={:?} (agnus={:?} denise={:?}) video={:?} rom={} floppy_drives={}",
        cfg.cpu,
        cfg.fpu,
        cfg.cpu_clock_mhz,
        cfg.chip_ram_bytes / 1024,
        cfg.fast_ram_bytes / 1024,
        cfg.slow_ram_bytes / 1024,
        cfg.z3_ram_bytes / 1024,
        cfg.zorro_boards.len(),
        cfg.chipset,
        cfg.agnus_revision,
        cfg.denise_revision,
        cfg.video_standard,
        cfg.rom_path.display(),
        cfg.floppy_connected.iter().filter(|&&connected| connected).count()
    );

    if matches!(cfg.chipset, Chipset::Aga) {
        info!(
            "chipset AGA: bitplanes/palette/FMODE fetch, sprites (wide fetch, manual \
             wide, SSCAN2/BSCAN2 scan doubling, BPLCON4 offsets) and CLXCON2 collisions \
             are implemented; residual gaps: 35 ns SHRES sprite output, AGA DDF fine \
             granularity, live collisions on the 6-plane decode (docs/internals/chipset.md)"
        );
    }

    if let Some(secs) = cli.live_audio_profile_secs {
        return run_live_audio_profile(secs);
    }

    // Best-effort realtime-like scheduling for the latency-critical threads.
    // Resolved once here (env var overrides the config) so the audio sink can
    // promote its callback thread and the pacer thread can be raised below.
    let realtime_priority = priority::requested(cfg.emulation.realtime_priority);
    if realtime_priority {
        info!("priority: realtime-like thread scheduling requested (best effort)");
    }
    let audio: Box<dyn AudioSink> = if let Some(ref wav_path) = cli.audio_wav {
        Box::new(WavSink::new(wav_path)?)
    } else if cli.audio_live {
        Box::new(CpalSink::new(realtime_priority)?)
    } else {
        Box::new(NullSink)
    };
    // Headless capture runs (screenshot / frame dump) advance the
    // deterministic core unthrottled; the interactive window paces to
    // wall-clock time. The emulated result is identical either way.
    let headless_capture = cli.screenshot_after.is_some()
        || cli.frame_dump.is_some()
        || cli.benchmark_until.is_some()
        || cli.gdb.is_some();
    let paced = !headless_capture;
    info!("emulation timing: deterministic core, paced={paced}");
    let mut emu = emulator::build_machine(&cfg, audio, paced, cli.load_state.is_some())?;
    if let Some(path) = &cli.load_state {
        let outcome = emu.load_state(path)?;
        info!(
            "save state loaded: {} ({}, resuming at {:.1}s emulated time)",
            path.display(),
            outcome.summary,
            emu.bus().emulated_seconds()
        );
    }
    // Arm reverse debugging (snapshot ring + optional one-shot "last writer"
    // watchpoint) from the COPPERLINE_DBG_RR*/RWATCH environment.
    if let Some(rr) = debugger::reverse_config_from_env() {
        if envcfg::var("COPPERLINE_RTC_FIXED_SECS").is_none() {
            warn!(
                "reverse debugging is armed but COPPERLINE_RTC_FIXED_SECS is unset; \
                 the guest RTC reads host wall-clock time, so replay may diverge. \
                 Set COPPERLINE_RTC_FIXED_SECS for deterministic reverse debugging."
            );
        }
        emu.enable_time_travel(rr.budget_mb, rr.interval_frames);
        if let Some(addr) = rr.watch_addr {
            emu.arm_reverse_watch(addr, rr.target_secs);
        }
    }
    if let Some(target_secs) = cli.benchmark_until {
        return run_headless_benchmark(emu, target_secs);
    }
    if let Some(gdb) = cli.gdb {
        return gdbstub::run(emu, gdb);
    }
    let disk_write_protected = std::array::from_fn(|idx| {
        cfg.floppy.drives[idx]
            .as_ref()
            .map(|d| d.write_protected)
            .unwrap_or(true)
    });
    video::set_pixel_aspect(config::resolve_pixel_aspect(cfg.pixel_aspect));
    let app = App::new(
        emu,
        cfg.emulation.power_on,
        cli.screenshot_after,
        cli.save_state_after,
        cli.frame_dump,
        cli.press_after,
        cli.click_after,
        cli.joy_after,
        cli.mouse_after,
        disk_insert_after,
        cli.record_input,
        cfg.floppy_playlists.clone(),
        disk_write_protected,
        config::resolve_overscan(cfg.overscan),
        config::resolve_phosphor(cfg.phosphor),
        cfg.emulation.warp_speed,
        cfg.joystick_input_mode,
        config::about_machine_lines(&cfg),
        raw_cfg,
    );

    // Elevate the thread that is about to run the event loop and the pacer.
    // Only when actually pacing to wall-clock time: headless capture advances
    // the core unthrottled, so priority buys it nothing.
    if realtime_priority && paced {
        priority::elevate_pacer_thread();
    }
    info!(
        "entering event loop. {HOST_SHORTCUT_MODIFIER_LABEL}+Q to quit, {HOST_SHORTCUT_MODIFIER_LABEL}+S to screenshot, {HOST_SHORTCUT_MODIFIER_LABEL}+G to capture/release mouse."
    );
    app.run()
}

/// Build the minimal placeholder machine that hosts the configuration screen
/// before a real machine is built. It needs no ROM file (a tiny in-memory ROM
/// that immediately stops) and a null audio sink so it claims no audio device
/// while it sits powered off behind the launcher; the user's chosen machine
/// replaces it when they press Run.
fn build_placeholder_machine() -> Result<Emulator> {
    use copperline::memory::{ROM_BASE, ROM_SIZE};
    let mut rom = vec![0u8; ROM_SIZE];
    // Reset vector: a small stack pointer and a PC just past it; the rest is a
    // STOP-then-NOP sled, so the placeholder CPU does nothing if ever stepped.
    rom[0..4].copy_from_slice(&0x0007_FFFEu32.to_be_bytes());
    rom[4..8].copy_from_slice(&(ROM_BASE as u32 + 8).to_be_bytes());
    for word in rom[8..].chunks_exact_mut(2) {
        word.copy_from_slice(&0x4E71u16.to_be_bytes());
    }
    let mem = Memory {
        chip_ram: vec![0u8; 512 * 1024],
        slow_ram: Vec::new(),
        rom,
        overlay: true,
        zorro: copperline::zorro::ZorroChain::default(),
        extended_rom: Vec::new(),
        extended_rom_base: 0,
        wcs: Vec::new(),
        wcs_write_protected: false,
    };
    let bus = Bus::new(
        mem,
        Paula::new(Box::new(StdoutSink::new()), Box::new(NullSink)),
        FloppyController::default(),
    );
    Emulator::new(
        bus,
        copperline::config::CpuModel::M68000,
        false,
        Default::default(),
        copperline::config::PacingBudget::Cycles,
        2,
        true,
    )
}

/// Open the machine-configuration screen (the launcher shown when Copperline is
/// started with no machine specified). A placeholder machine sits powered off
/// behind the panel until the user presses Run, which builds and starts their
/// chosen machine in place.
fn run_configuration_screen(raw_cfg: config::RawConfig) -> Result<()> {
    info!("no machine specified; opening the configuration screen");
    let emu = build_placeholder_machine()?;
    video::set_pixel_aspect(config::resolve_pixel_aspect(config::PixelAspect::Tv));
    let mut app = App::new(
        emu,
        false,
        None,
        None,
        None,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        std::array::from_fn(|_| Vec::new()),
        [true; 4],
        config::resolve_overscan(config::Overscan::Tv),
        config::resolve_phosphor(0.0),
        config::WarpSpeed::default(),
        config::JoystickInputMode::default(),
        vec!["Configure a machine, then press Run.".to_string()],
        raw_cfg,
    );
    app.open_launcher();
    app.run()
}

/// Whether to show the configuration screen instead of booting: only on a bare
/// interactive launch with nothing specified (no config file, ROM, overrides,
/// scripted input, headless capture, or save-state load), and with live audio
/// (the launcher's Run path uses the live audio sink).
fn launcher_requested(cli: &CliArgs) -> bool {
    cli.config_path.is_none()
        && cli.rom_path.is_none()
        && cli.overrides.is_empty()
        && !Path::new("copperline.toml").exists()
        && cli.screenshot_after.is_none()
        && cli.save_state_after.is_none()
        && cli.frame_dump.is_none()
        && cli.benchmark_until.is_none()
        && cli.gdb.is_none()
        && cli.load_state.is_none()
        && cli.press_after.is_empty()
        && cli.click_after.is_empty()
        && cli.joy_after.is_empty()
        && cli.mouse_after.is_empty()
        && cli.disk_insert_after.is_empty()
        && cli.record_input.is_none()
        && cli.audio_wav.is_none()
        && cli.audio_live
}

fn run_live_audio_profile(secs: f32) -> Result<()> {
    info!(
        "audio profile mode: running Paula DMA to cpal for {:.3}s without window rendering",
        secs
    );
    // This diagnostic mode loads no config, so the realtime knob is env-only.
    let audio = Box::new(CpalSink::new(priority::requested(false))?);
    let mut paula = Paula::new(Box::new(StdoutSink::new()), audio);
    paula.set_led_filter_enabled(true);

    let mut chip_ram = vec![0u8; 64];
    chip_ram[0] = 0x40;
    chip_ram[1] = 0xC0;
    chip_ram[2] = 0x20;
    chip_ram[3] = 0xE0;

    paula.write_audio_reg(0x00, 0);
    paula.write_audio_reg(0x02, 0);
    paula.write_audio_reg(0x04, 1);
    paula.write_audio_reg(0x06, 400);
    paula.write_audio_reg(0x08, 64);
    paula.write_audio_reg(0x10, 0);
    paula.write_audio_reg(0x12, 2);
    paula.write_audio_reg(0x14, 1);
    paula.write_audio_reg(0x16, 512);
    paula.write_audio_reg(0x18, 48);

    let dmacon = DMACON_DMAEN | 0x0003;
    let quantum = Duration::from_millis(5);
    let quantum_cck = (PAULA_CLOCK_HZ as f64 * quantum.as_secs_f64())
        .round()
        .clamp(1.0, u32::MAX as f64) as u32;
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f32(secs);
    let mut chunks = 0u64;

    while Instant::now() < deadline {
        let chunk_started = Instant::now();
        let _ = advance_paula_profile_audio(&mut paula, quantum_cck, dmacon, &chip_ram);
        chunks = chunks.saturating_add(1);
        if let Some(wait) = quantum.checked_sub(chunk_started.elapsed()) {
            std::thread::sleep(wait);
        }
    }

    let elapsed = started.elapsed().as_secs_f64();
    info!(
        "audio profile mode complete: elapsed={:.3}s chunks={} quantum_cck={}",
        elapsed, chunks, quantum_cck
    );
    Ok(())
}

fn advance_paula_profile_audio(paula: &mut Paula, cck: u32, dmacon: u16, chip_ram: &[u8]) -> u16 {
    let mut irq = 0;
    for _ in 0..cck {
        irq |= paula.advance_audio(0, dmacon);
        for channel in 0..4 {
            while let Some(request) = paula.audio_dma_request(channel) {
                let word = read_profile_audio_word(chip_ram, request.address);
                irq |= paula.grant_audio_dma(channel, word);
            }
        }
        irq |= paula.advance_audio(1, dmacon);
        for channel in 0..4 {
            while let Some(request) = paula.audio_dma_request(channel) {
                let word = read_profile_audio_word(chip_ram, request.address);
                irq |= paula.grant_audio_dma(channel, word);
            }
        }
    }
    irq
}

fn read_profile_audio_word(chip_ram: &[u8], address: u32) -> u16 {
    if chip_ram.is_empty() {
        return 0;
    }
    let off = (address as usize) % chip_ram.len();
    ((chip_ram[off] as u16) << 8) | chip_ram[(off + 1) % chip_ram.len()] as u16
}

/// Load the config, returning both the validated [`Config`] used to build the
/// machine and the raw TOML view it came from. The configuration screen keeps
/// the raw view so its "Machine Configuration..." menu item can reopen showing
/// the running machine's settings.
fn load_config(
    explicit: Option<&Path>,
    overrides: &ConfigOverrides,
) -> Result<(Config, config::RawConfig)> {
    // Resolve which file (if any) backs the config: the explicit --config
    // path, then ./copperline.toml if present, otherwise the built-in
    // defaults. CLI overrides layer on top of whichever it is.
    let default = Path::new("copperline.toml");
    let path = if explicit.is_some() {
        explicit
    } else if default.exists() {
        info!("loading config from {}", default.display());
        Some(default)
    } else {
        None
    };
    let raw = Config::load_raw(path, overrides)?;
    let cfg = Config::try_from(raw.clone())?;
    Ok((cfg, raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CliArgs> {
        parse_args_from(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn placeholder_machine_builds() {
        // The configuration screen's host machine must build without any ROM
        // file or audio device (it sits powered off behind the launcher).
        build_placeholder_machine().expect("placeholder machine builds");
    }

    #[test]
    fn launcher_shows_only_when_nothing_is_specified() {
        // A bare interactive launch (no config file present in this dir under
        // test) opens the configuration screen...
        let bare = parse(&[]).unwrap();
        assert!(launcher_requested(&bare));
        // ...but specifying a ROM, an override, or a headless capture boots
        // directly instead.
        assert!(!launcher_requested(&parse(&["KICK.ROM"]).unwrap()));
        assert!(!launcher_requested(&parse(&["--model", "A1200"]).unwrap()));
        assert!(!launcher_requested(
            &parse(&["--screenshot-after", "5", "out.png"]).unwrap()
        ));
        assert!(!launcher_requested(&parse(&["--noaudio"]).unwrap()));
    }

    fn temp_script(name: &str, contents: &str) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "copperline-script-{}-{unique}-{name}.clscript",
            std::process::id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn mouse_after_parses_signed_deltas() -> Result<()> {
        let args = parse(&["--mouse-after", "1.5", "-3", "10"])?;
        assert_eq!(args.mouse_after, vec![(1.5, -3, 10)]);
        Ok(())
    }

    #[test]
    fn script_file_expands_to_the_equivalent_flags() -> Result<()> {
        let path = temp_script(
            "ok",
            "# recorded session\n\
             key-after 14.0 ctrl 500\n\
             press-after 14.1 0x63\n\
             \n\
             click-after 5.0 left 100\n\
             joy-after 60.0 red 300\n\
             mouse-after 1.020 -3 10\n\
             insert-disk-after 30.0 df1 \"/tmp/with space.adf\"\n",
        );
        let args = parse(&["--script", path.to_str().unwrap()])?;
        assert_eq!(args.press_after.len(), 2);
        assert_eq!(args.press_after[0].hold_ms, 500);
        assert_eq!(args.click_after, vec![(5.0, MouseButtonKind::Left, 100)]);
        assert_eq!(args.joy_after, vec![(60.0, JoyButtonKind::Red, 300)]);
        assert_eq!(args.mouse_after, vec![(1.02, -3, 10)]);
        assert_eq!(
            args.disk_insert_after,
            vec![CliDiskInsert::Explicit(DiskInsertSpec {
                secs: 30.0,
                drive_idx: 1,
                path: PathBuf::from("/tmp/with space.adf"),
                write_protected: true,
            })]
        );
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn script_files_reject_non_input_directives() {
        // Anything outside the scripted-input set is refused, including
        // nesting another script.
        for line in [
            "config /tmp/evil.toml",
            "script /tmp/other",
            "load-state /tmp/x",
        ] {
            let path = temp_script("bad", line);
            let err = parse(&["--script", path.to_str().unwrap()]).unwrap_err();
            assert!(
                err.to_string().contains("not a scripted-input directive"),
                "{line}: {err}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn script_lines_with_unterminated_quotes_are_rejected() {
        let path = temp_script("quote", "insert-disk-after 1.0 df0 \"/tmp/unterminated\n");
        let err = parse(&["--script", path.to_str().unwrap()]).unwrap_err();
        assert!(err.to_string().contains("unterminated quote"), "{err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn recorded_script_round_trips_through_the_parser() -> Result<()> {
        // What the recorder emits must come back as the same scheduled
        // events through --script.
        let mut rec = copperline::inputrec::InputRecorder::new(0.0);
        let mut input = copperline::bus::InputState::default();
        rec.observe(&input, 1.0);
        rec.record_key(0x45, true, 1.5);
        rec.record_key(0x45, false, 1.75);
        input.mouse_x_port1 = 5;
        input.lmb_port1 = true;
        rec.observe(&input, 2.0);
        input.lmb_port1 = false;
        rec.observe(&input, 2.5);
        rec.record_disk_insert(0, Path::new("/tmp/demo.adf"), 3.0);
        let path = temp_script("roundtrip", &rec.finish());

        let args = parse(&["--script", path.to_str().unwrap()])?;
        assert_eq!(args.press_after.len(), 1);
        assert_eq!(args.press_after[0].rawkey, 0x45);
        assert_eq!(args.press_after[0].hold_ms, 250);
        assert_eq!(args.mouse_after, vec![(2.0, 5, 0)]);
        assert_eq!(args.click_after, vec![(2.0, MouseButtonKind::Left, 500)]);
        assert_eq!(args.disk_insert_after.len(), 1);
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn audio_is_enabled_by_default() -> Result<()> {
        let args = parse(&[])?;
        assert!(args.audio_live);
        assert!(args.audio_wav.is_none());
        Ok(())
    }

    #[test]
    fn noaudio_disables_live_audio() -> Result<()> {
        let args = parse(&["--noaudio"])?;
        assert!(!args.audio_live);
        assert!(args.audio_wav.is_none());
        Ok(())
    }

    #[test]
    fn audio_wav_selects_wav_output_without_live_audio() -> Result<()> {
        let args = parse(&["--audio-wav", "/tmp/out.wav"])?;
        assert!(!args.audio_live);
        assert_eq!(args.audio_wav, Some(PathBuf::from("/tmp/out.wav")));
        Ok(())
    }

    #[test]
    fn explicit_audio_conflicts_with_audio_wav() {
        let err = parse(&["--audio", "--audio-wav", "/tmp/out.wav"]).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "{err:#}");
    }

    #[test]
    fn live_audio_profile_mode_parses_duration_and_requires_live_audio() -> Result<()> {
        let args = parse(&["--profile-live-audio", "0.25"])?;
        assert_eq!(args.live_audio_profile_secs, Some(0.25));
        assert!(args.audio_live);
        assert!(args.audio_wav.is_none());

        let err = parse(&["--profile-live-audio", "0.25", "--noaudio"]).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "{err:#}");
        Ok(())
    }

    #[test]
    fn benchmark_until_parses_and_defaults_to_null_audio() -> Result<()> {
        let args = parse(&["--benchmark-until", "85.4"])?;
        assert_eq!(args.benchmark_until, Some(85.4));
        assert!(!args.audio_live);
        assert!(args.audio_wav.is_none());
        validate_benchmark_args(&args)?;
        Ok(())
    }

    #[test]
    fn benchmark_until_preserves_explicit_live_audio() -> Result<()> {
        let args = parse(&["--benchmark-until", "85.4", "--audio"])?;
        assert_eq!(args.benchmark_until, Some(85.4));
        assert!(args.audio_live);
        validate_benchmark_args(&args)?;
        Ok(())
    }

    #[test]
    fn benchmark_until_rejects_window_scheduled_work() -> Result<()> {
        let args = parse(&["--benchmark-until", "85.4", "--press-after", "1.0", "ctrl"])?;
        let err = validate_benchmark_args(&args).unwrap_err();
        assert!(err.to_string().contains("scheduled input"), "{err:#}");

        let args = parse(&["--benchmark-until", "85.4", "--profile-live-audio", "0.1"])?;
        let err = validate_benchmark_args(&args).unwrap_err();
        assert!(err.to_string().contains("--profile-live-audio"), "{err:#}");

        let args = parse(&[
            "--benchmark-until",
            "85.4",
            "--screenshot-after",
            "85.4",
            "/tmp/x",
        ])?;
        let err = validate_benchmark_args(&args).unwrap_err();
        assert!(err.to_string().contains("--screenshot-after"), "{err:#}");
        Ok(())
    }

    #[test]
    fn gdb_mode_parses_and_defaults_to_null_audio() -> Result<()> {
        let args = parse(&["--gdb", ":2345"])?;
        assert_eq!(
            args.gdb,
            Some(copperline::gdbstub::Config::new(":2345".to_string()))
        );
        assert!(!args.audio_live);
        validate_gdb_args(&args)?;
        Ok(())
    }

    #[test]
    fn gdb_mode_rejects_window_scheduled_work() -> Result<()> {
        let args = parse(&["--gdb", ":2345", "--press-after", "1.0", "ctrl"])?;
        let err = validate_gdb_args(&args).unwrap_err();
        assert!(err.to_string().contains("scheduled input"), "{err:#}");

        let args = parse(&[
            "--gdb",
            ":2345",
            "--screenshot-after",
            "1.0",
            "/tmp/gdb.png",
        ])?;
        let err = validate_gdb_args(&args).unwrap_err();
        assert!(err.to_string().contains("--screenshot-after"), "{err:#}");
        Ok(())
    }

    #[test]
    fn frame_dump_options_parse() -> Result<()> {
        let args = parse(&[
            "--dump-frames",
            "/tmp/frontier-clouds",
            "--dump-start",
            "18.5",
            "--dump-count",
            "42",
        ])?;
        assert_eq!(
            args.frame_dump,
            Some(FrameDumpSpec {
                dir: PathBuf::from("/tmp/frontier-clouds"),
                start_secs: 18.5,
                count: 42,
            })
        );
        Ok(())
    }

    #[test]
    fn frame_dump_requires_count_and_directory() {
        let err = parse(&["--dump-frames", "/tmp/frontier-clouds"]).unwrap_err();
        assert!(
            err.to_string().contains("--dump-count"),
            "unexpected error: {err:#}"
        );

        let err = parse(&["--dump-count", "10"]).unwrap_err();
        assert!(
            err.to_string().contains("--dump-frames"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn insert_disk_after_parses_explicit_drive_and_path() -> Result<()> {
        let args = parse(&["--insert-disk-after", "10", "df0", "demo-disk.adf"])?;
        assert_eq!(
            args.disk_insert_after,
            vec![CliDiskInsert::Explicit(DiskInsertSpec {
                secs: 10.0,
                drive_idx: 0,
                path: PathBuf::from("demo-disk.adf"),
                write_protected: true,
            })]
        );
        Ok(())
    }

    #[test]
    fn floppy_drive_count_override_parses_with_alias() -> Result<()> {
        assert_eq!(
            parse(&["--floppy-drives", "2"])?.overrides.floppy_drives,
            Some(2)
        );
        assert_eq!(
            parse(&["--fdd-drives", "4"])?.overrides.floppy_drives,
            Some(4)
        );
        let err = parse(&["--floppy-drives", "0"]).unwrap_err();
        assert!(err.to_string().contains("from 1 to 4"), "{err:#}");
        Ok(())
    }

    #[test]
    fn defer_disk_insert_parses_configured_drive() -> Result<()> {
        let args = parse(&["--defer-disk-insert", "10", "df0:"])?;
        assert_eq!(
            args.disk_insert_after,
            vec![CliDiskInsert::Configured {
                secs: 10.0,
                drive_idx: 0,
            }]
        );
        Ok(())
    }

    #[test]
    fn deferred_configured_disk_insert_starts_drive_empty() -> Result<()> {
        let mut cfg = Config::default();
        cfg.floppy.drives[0] = Some(copperline::config::FloppyDriveConfig {
            path: PathBuf::from("demo-disk.adf"),
            write_protected: true,
        });

        let inserts = resolve_disk_insert_after(
            &mut cfg,
            vec![CliDiskInsert::Configured {
                secs: 10.0,
                drive_idx: 0,
            }],
        )?;

        assert!(cfg.floppy.drives[0].is_none());
        assert_eq!(
            inserts,
            vec![DiskInsertSpec {
                secs: 10.0,
                drive_idx: 0,
                path: PathBuf::from("demo-disk.adf"),
                write_protected: true,
            }]
        );
        Ok(())
    }

    #[test]
    fn scheduled_disk_insert_requires_connected_drive() {
        let mut cfg = Config::default();
        let err = resolve_disk_insert_after(
            &mut cfg,
            vec![CliDiskInsert::Explicit(DiskInsertSpec {
                secs: 10.0,
                drive_idx: 1,
                path: PathBuf::from("demo-disk.adf"),
                write_protected: true,
            })],
        )
        .unwrap_err();
        assert!(err.to_string().contains("connected drive"), "{err:#}");

        cfg.floppy_connected[1] = true;
        let inserts = resolve_disk_insert_after(
            &mut cfg,
            vec![CliDiskInsert::Explicit(DiskInsertSpec {
                secs: 10.0,
                drive_idx: 1,
                path: PathBuf::from("demo-disk.adf"),
                write_protected: true,
            })],
        )
        .unwrap();
        assert_eq!(inserts[0].drive_idx, 1);
    }

    #[test]
    fn press_after_accepts_named_keys_with_default_hold() -> Result<()> {
        let args = parse(&["--press-after", "1.5", "ctrl"])?;
        assert_eq!(
            args.press_after,
            vec![KeyPressSpec {
                secs: 1.5,
                rawkey: 0x63,
                hold_ms: DEFAULT_KEY_HOLD_MS,
            }]
        );
        Ok(())
    }

    #[test]
    fn key_after_accepts_named_modifier_and_hold_duration() -> Result<()> {
        let args = parse(&["--key-after", "2.0", "lami", "750"])?;
        assert_eq!(
            args.press_after,
            vec![KeyPressSpec {
                secs: 2.0,
                rawkey: 0x66,
                hold_ms: 750,
            }]
        );
        Ok(())
    }

    #[test]
    fn press_after_still_accepts_raw_numeric_keys() -> Result<()> {
        let args = parse(&["--press-after", "1.0", "0x04"])?;
        assert_eq!(args.press_after[0].rawkey, 0x04);
        Ok(())
    }

    #[test]
    fn machine_override_flags_parse_into_config_overrides() -> Result<()> {
        let args = parse(&[
            "--model",
            "A1200",
            "--cpu",
            "68030",
            "--cpu-clock",
            "50",
            "--fpu",
            "--chip",
            "2M",
            "--fast",
            "8M",
            "--slow",
            "512K",
            "--floppy-drives",
            "3",
            "--chipset",
            "AGA",
        ])?;
        assert_eq!(args.overrides.model.as_deref(), Some("A1200"));
        assert_eq!(args.overrides.cpu.as_deref(), Some("68030"));
        assert_eq!(args.overrides.cpu_clock_mhz, Some(50.0));
        assert_eq!(args.overrides.fpu, Some(true));
        assert_eq!(args.overrides.chip.as_deref(), Some("2M"));
        assert_eq!(args.overrides.fast.as_deref(), Some("8M"));
        assert_eq!(args.overrides.slow.as_deref(), Some("512K"));
        assert_eq!(args.overrides.floppy_drives, Some(3));
        assert_eq!(args.overrides.chipset.as_deref(), Some("AGA"));
        Ok(())
    }

    #[test]
    fn no_fpu_flag_sets_override_false_and_default_is_unset() -> Result<()> {
        assert_eq!(parse(&[])?.overrides.fpu, None);
        assert_eq!(parse(&["--no-fpu"])?.overrides.fpu, Some(false));
        Ok(())
    }

    #[test]
    fn cpu_clock_rejects_non_numeric() {
        let err = parse(&["--cpu-clock", "fast"]).unwrap_err();
        assert!(err.to_string().contains("--cpu-clock"), "{err:#}");
    }
}
