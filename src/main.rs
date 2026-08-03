// SPDX-License-Identifier: GPL-3.0-or-later

//! Copperline: Amiga emulator.
//!
//! Usage: copperline [--config FILE] [ROM]
//!   If no --config is given, looks for ./copperline.toml.
//!   If no ROM is given (neither argument nor `rom =` in the config), boots
//!   the bundled AROS open-source Kickstart replacement (see src/romsearch.rs).

use anyhow::{anyhow, Result};
use copperline::{config, crashlog, debugger, emulator, envcfg, gamepad, gdbstub, priority, video};
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
    /// `--control ADDR`: run the headless Copperline Control Protocol
    /// server (JSON-RPC over loopback TCP), pausing at reset until a
    /// client resumes. `--control-token`/`--control-info` refine it.
    /// Kept as the raw listen address so the CLI parses without the
    /// `control` feature; a build without it rejects the flags in
    /// validation, and `main` assembles the server config at dispatch.
    pub control: Option<String>,
    /// `--control-gui ADDR`: attach a control server to the normal
    /// windowed session instead of owning the machine.
    pub control_gui: Option<String>,
    /// `--control-token TOKEN` / `--control-info PATH` for either mode.
    pub control_token: Option<String>,
    pub control_info: Option<PathBuf>,
    /// Dump consecutive rendered frames after an emulated-time delay. This
    /// is intended for debugging flicker and frame-to-frame palette
    /// changes that a single screenshot cannot show.
    pub frame_dump: Option<FrameDumpSpec>,
    /// `--waveform PATH` (+ `--wave-trigger/--wave-duration/--wave-signals`):
    /// arm a trigger-based VCD logic-analyser capture of internal chipset
    /// signals for GTKWave (see docs/debugger/waveform.md).
    pub waveform: Option<copperline::waveform::WaveOptions>,
    /// Scripted key presses to inject after the window opens. Useful
    /// for headless testing of menus and modifier chords.
    pub press_after: Vec<KeyPressSpec>,
    /// `--click-after SECS BUTTON DURATION_MS [PORT]`: at SECS seconds
    /// after the window opens, press the named mouse button
    /// (left/right/middle), hold for DURATION_MS, then release. The
    /// optional trailing PORT (1 or 2) names the controller port,
    /// defaulting to 1; the tuple carries it 0-based. Useful for headless
    /// testing of the mouse-button-driven wait prompts.
    pub click_after: Vec<(f32, MouseButtonKind, u32, u8)>,
    /// `--joy-after SECS BUTTON DURATION_MS [PORT]`: at SECS emulated
    /// seconds, press a joystick / CD32-pad control (up/down/left/right/
    /// red|fire/blue/green/yellow/play/rwd/ffw), hold for DURATION_MS,
    /// then release. PORT defaults to 2 (carried 0-based). Useful for
    /// headless testing of joystick-driven titles, especially CD32 games
    /// whose pad otherwise needs a calibrated physical gamepad.
    pub joy_after: Vec<(f32, JoyButtonKind, u32, u8)>,
    /// `--mouse-after SECS DX DY [PORT]`: at SECS emulated seconds, apply
    /// a relative mouse motion of (DX, DY) counter steps. PORT defaults
    /// to 1 (carried 0-based). Emitted by the input recorder one event
    /// per frame of recorded movement.
    pub mouse_after: Vec<(f32, i32, i32, u8)>,
    /// `--mouse-to-after SECS X Y [PORT]`: at SECS emulated seconds,
    /// servo the guest pointer to presented-pixel (X, Y) -- the same
    /// coordinates a screenshot is measured in -- by watching sprite 0
    /// and correcting relative motion until it lands. PORT defaults to 1
    /// (carried 0-based). See `src/pointer.rs` for why absolute pointer
    /// positioning has to be closed-loop.
    pub mouse_to_after: Vec<(f32, i32, i32, u8)>,
    /// `--pot-after SECS X Y [PORT]`: at SECS emulated seconds, set an
    /// analogue controller's stick/paddle position (each axis 0-255, the
    /// count POTxDAT latches). PORT defaults to 2 (carried 0-based).
    pub pot_after: Vec<(f32, u8, u8, u8)>,
    /// `--record-input PATH`: record every input event that reaches the
    /// emulated machine for the whole run and write the scripted-input
    /// file to PATH on exit (the windowed toggle is the host shortcut
    /// modifier plus Shift+R).
    pub record_input: Option<PathBuf>,
    /// Scripted floppy image insertion. This supports both explicit
    /// paths and deferring a disk image already configured in the TOML.
    pub disk_insert_after: Vec<CliDiskInsert>,
    /// Scripted CD swaps: (SECS, image path) pairs from --insert-cd-after,
    /// landing in whichever CD drive the machine has (CDTV, CD32, or a
    /// SCSI CD-ROM unit).
    pub cd_insert_after: Vec<(f32, PathBuf)>,
    /// Real-time stereo audio output through cpal. Enabled by default;
    /// `--noaudio` disables it, and `--audio-wav` selects WAV output.
    pub audio_live: bool,
    /// Whether `--audio` was passed explicitly. When set, live audio is forced
    /// on regardless of `[audio] output_enabled`; otherwise that config key (the
    /// GUI "Disabled" option) can turn default-on audio off.
    pub audio_live_forced: bool,
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
    /// `--list-audio-devices`: print the host audio output devices and exit.
    pub list_audio_devices: bool,
    /// `--list-net-interfaces`: print adapters usable for bridging and exit.
    pub list_net_interfaces: bool,
    /// Linux companion helper setup action: install, uninstall, or status.
    pub net_helper_action: Option<String>,
    /// `--sampler-list-audio-inputs`: print the host audio input devices (for
    /// `--sampler-audio-input`) and exit.
    pub list_sampler_inputs: bool,
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
const SCRIPT_DIRECTIVES: [&str; 11] = [
    "press-after",
    "key-after",
    "hold-key-after",
    "click-after",
    "joy-after",
    "mouse-after",
    "mouse-to-after",
    "pot-after",
    "insert-disk-after",
    "defer-disk-insert",
    "insert-cd-after",
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

/// Consume the optional trailing PORT token (exactly "1" or "2") a
/// scripted-input flag accepts after its fixed arguments, returning the
/// 0-based port index; anything else leaves the token for the main loop
/// and yields the flag's traditional default port. (A positional ROM/disk
/// path literally named "1" or "2" therefore cannot directly follow one
/// of these flags; name it "./1" instead.)
fn take_port_token(
    args: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    default_port: u8,
) -> u8 {
    match args.peek().map(String::as_str) {
        Some("1") => {
            args.next();
            0
        }
        Some("2") => {
            args.next();
            1
        }
        _ => default_port - 1,
    }
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
    let mut control_listen: Option<String> = None;
    let mut control_gui_listen: Option<String> = None;
    let mut control_token: Option<String> = None;
    let mut control_info: Option<PathBuf> = None;
    let mut dump_dir: Option<PathBuf> = None;
    let mut dump_start_secs: f32 = 0.0;
    let mut dump_count: Option<u32> = None;
    let mut press_after: Vec<KeyPressSpec> = Vec::new();
    let mut click_after: Vec<(f32, MouseButtonKind, u32, u8)> = Vec::new();
    let mut joy_after: Vec<(f32, JoyButtonKind, u32, u8)> = Vec::new();
    let mut mouse_after: Vec<(f32, i32, i32, u8)> = Vec::new();
    let mut mouse_to_after: Vec<(f32, i32, i32, u8)> = Vec::new();
    let mut pot_after: Vec<(f32, u8, u8, u8)> = Vec::new();
    let mut record_input: Option<PathBuf> = None;
    let mut wave_path: Option<PathBuf> = None;
    let mut wave_trigger: Option<copperline::waveform::Trigger> = None;
    let mut wave_duration: Option<copperline::waveform::WaveDuration> = None;
    let mut wave_signals: Option<copperline::waveform::SignalSet> = None;
    let mut disk_insert_after: Vec<CliDiskInsert> = Vec::new();
    let mut cd_insert_after: Vec<(f32, PathBuf)> = Vec::new();
    let mut audio_live = true;
    let mut explicit_audio_live = false;
    let mut explicit_noaudio = false;
    let mut audio_wav: Option<PathBuf> = None;
    let mut live_audio_profile_secs: Option<f32> = None;
    let mut calibrate_gamepad = false;
    let mut list_midi = false;
    let mut list_audio_devices = false;
    let mut list_net_interfaces = false;
    let mut net_helper_action: Option<String> = None;
    let mut list_sampler_inputs = false;
    let mut overrides = ConfigOverrides::default();
    let mut args = args.into_iter().peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--calibrate-gamepad" => {
                calibrate_gamepad = true;
            }
            "--list-midi" => {
                list_midi = true;
            }
            "--list-audio-devices" => {
                list_audio_devices = true;
            }
            "--list-net-interfaces" => {
                list_net_interfaces = true;
            }
            "--install-net-helper" => {
                if net_helper_action.is_some() {
                    return Err(anyhow!("only one network-helper action may be requested"));
                }
                net_helper_action = Some("install".to_string());
            }
            "--uninstall-net-helper" => {
                if net_helper_action.is_some() {
                    return Err(anyhow!("only one network-helper action may be requested"));
                }
                net_helper_action = Some("uninstall".to_string());
            }
            "--net-helper-status" => {
                if net_helper_action.is_some() {
                    return Err(anyhow!("only one network-helper action may be requested"));
                }
                net_helper_action = Some("status".to_string());
            }
            "--sampler-list-audio-inputs" => {
                list_sampler_inputs = true;
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
            "--full-screen" => {
                overrides.full_screen = Some(true);
            }
            "--windowed" => {
                overrides.full_screen = Some(false);
            }
            "--show-status-bar" => {
                overrides.status_bar = Some(true);
            }
            "--mt32-control-rom" => {
                overrides.mt32_control_rom = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--mt32-control-rom requires a path"))?,
                );
            }
            "--mt32-pcm-rom" => {
                overrides.mt32_pcm_rom = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--mt32-pcm-rom requires a path"))?,
                );
            }
            "--mt32-panel" => {
                overrides.mt32_panel = Some(true);
            }
            "--menu-scale" => {
                overrides.menu_scale = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--menu-scale requires 1x or 2x"))?,
                );
            }
            "--hide-status-bar" => {
                overrides.status_bar = Some(false);
            }
            "--perf-overlay" => {
                overrides.perf_overlay = Some(true);
            }
            "--cpu-clock" => {
                let mhz: f64 = next_arg(
                    &mut args,
                    "--cpu-clock requires MHZ",
                    "--cpu-clock MHZ must be a number",
                )?;
                overrides.cpu_clock_mhz = Some(mhz);
            }
            "--jit" => {
                overrides.cpu_jit = Some(true);
            }
            "--no-jit" => {
                overrides.cpu_jit = Some(false);
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
            "--motherboard" => {
                overrides.motherboard =
                    Some(args.next().ok_or_else(|| {
                        anyhow!("--motherboard requires a size (e.g. 0, 4M, 16M)")
                    })?);
            }
            "--accelerator" => {
                overrides.accelerator =
                    Some(args.next().ok_or_else(|| {
                        anyhow!("--accelerator requires a size (e.g. 0, 32M, 128M)")
                    })?);
            }
            "--floppy-drives" | "--fdd-drives" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--floppy-drives requires COUNT (1-4)"))?;
                overrides.floppy_drives = Some(parse_floppy_drive_count(&value)?);
            }
            // Absent from a build without the feature, so an unknown-argument
            // error names it rather than the flag quietly doing nothing.
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge" => {
                const USAGE: &str = "--floppy-bridge requires DFN INTERFACE \
                                     (drawbridge, greaseweazle, supercardpro, or off)";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge")?;
                let interface = args.next().ok_or_else(|| anyhow!(USAGE))?;
                overrides.floppy_bridge[idx] = Some(interface);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-port" => {
                const USAGE: &str = "--floppy-bridge-port requires DFN PORT";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-port")?;
                overrides.floppy_bridge_port[idx] =
                    Some(args.next().ok_or_else(|| anyhow!(USAGE))?);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-cable" => {
                const USAGE: &str = "--floppy-bridge-cable requires DFN CABLE \
                                     (a or b for a PC cable, 0-3 for Shugart)";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-cable")?;
                overrides.floppy_bridge_cable[idx] =
                    Some(args.next().ok_or_else(|| anyhow!(USAGE))?);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-mode" => {
                const USAGE: &str = "--floppy-bridge-mode requires DFN MODE \
                                     (normal, compatible, or stalling)";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-mode")?;
                overrides.floppy_bridge_mode[idx] =
                    Some(args.next().ok_or_else(|| anyhow!(USAGE))?);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-density" => {
                const USAGE: &str = "--floppy-bridge-density requires DFN DENSITY \
                                     (auto, dd, or hd)";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-density")?;
                overrides.floppy_bridge_density[idx] =
                    Some(args.next().ok_or_else(|| anyhow!(USAGE))?);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-speed" => {
                const USAGE: &str =
                    "--floppy-bridge-speed requires DFN PERCENT (100, 125, 150, 175, or 200)";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let percent_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-speed")?;
                overrides.floppy_bridge_speed[idx] = Some(parse_floppy_bridge_speed(&percent_s)?);
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-auto-cache" => {
                const USAGE: &str = "--floppy-bridge-auto-cache requires DFN";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-auto-cache")?;
                overrides.floppy_bridge_auto_cache[idx] = true;
            }
            #[cfg(feature = "floppybridge")]
            "--floppy-bridge-writable" => {
                const USAGE: &str = "--floppy-bridge-writable requires DFN";
                let drive_s = args.next().ok_or_else(|| anyhow!(USAGE))?;
                let idx = parse_floppy_drive_idx(&drive_s, "--floppy-bridge-writable")?;
                overrides.floppy_bridge_writable[idx] = true;
            }
            "--floppy-speed" | "--fdd-speed" => {
                let value = args.next().ok_or_else(|| {
                    anyhow!("--floppy-speed requires PERCENT (100, 200, 400, 800, or 0 for turbo)")
                })?;
                overrides.floppy_speed = Some(parse_floppy_speed(&value)?);
            }
            "--rtc-time" => {
                overrides.rtc_time = Some(args.next().ok_or_else(|| {
                    anyhow!("--rtc-time requires Unix seconds or \"YYYY-MM-DD HH:MM[:SS]\"")
                })?);
            }
            "--rtc-frozen" => {
                overrides.rtc_frozen = Some(true);
            }
            "--joystick" => {
                overrides.joystick = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--joystick requires a mode (gamepad/keyboard)"))?,
                );
            }
            "--port1" => {
                overrides.port1 = Some(args.next().ok_or_else(|| {
                    anyhow!("--port1 requires a device (mouse/joystick/cd32/analogue/none)")
                })?);
            }
            "--port2" => {
                overrides.port2 = Some(args.next().ok_or_else(|| {
                    anyhow!("--port2 requires a device (mouse/joystick/cd32/analogue/none)")
                })?);
            }
            "--autofire" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--autofire requires a rate in Hz (0 = off)"))?;
                overrides.autofire_hz = Some(
                    value
                        .parse::<u8>()
                        .map_err(|_| anyhow!("--autofire rate must be a whole number of Hz"))?,
                );
            }
            "--serial" => {
                overrides.serial = Some(args.next().ok_or_else(|| {
                    anyhow!("--serial requires a mode (off/stdout/midi/tcp/tcp-connect/pty)")
                })?);
            }
            "--serial-connect" => {
                overrides.serial_connect =
                    Some(args.next().ok_or_else(|| {
                        anyhow!("--serial-connect requires an address (host:port)")
                    })?);
            }
            "--a2065-net" => {
                overrides.a2065_net = Some(args.next().ok_or_else(|| {
                    anyhow!("--a2065-net requires a backend (none/loopback/nat/bridge)")
                })?);
            }
            "--a2065-interface" => {
                overrides.a2065_interface = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--a2065-interface requires an adapter name"))?,
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
            "--parallel" => {
                overrides.parallel = Some(args.next().ok_or_else(|| {
                    anyhow!("--parallel requires a device (none/printer/sampler)")
                })?);
            }
            "--sampler-audio-input" => {
                overrides.sampler_input = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--sampler-audio-input requires a device name"))?,
                );
            }
            "--sampler-input-gain" => {
                let gain: f32 = next_arg(
                    &mut args,
                    "--sampler-input-gain requires a value in dB (e.g. 0, 6, -6)",
                    "--sampler-input-gain must be a number",
                )?;
                overrides.sampler_gain = Some(gain);
            }
            "--audio-device" => {
                overrides.audio_device = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--audio-device requires a device name"))?,
                );
            }
            "--audio-channel-mode" => {
                overrides.audio_channel_mode = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--audio-channel-mode requires stereo or mono"))?,
                );
            }
            "--audio-filter" => {
                overrides.audio_filter = Some(
                    args.next()
                        .ok_or_else(|| anyhow!("--audio-filter requires auto, on, or off"))?,
                );
            }
            "--audio-stereo-separation" => {
                let v = args.next().ok_or_else(|| {
                    anyhow!("--audio-stereo-separation requires a percent (0-100)")
                })?;
                overrides.audio_stereo_separation =
                    Some(v.parse::<u16>().map_err(|_| {
                        anyhow!("--audio-stereo-separation must be a number 0-100")
                    })?);
            }
            "--mouse-sensitivity" => {
                let v = args
                    .next()
                    .ok_or_else(|| anyhow!("--mouse-sensitivity requires a value (0-100)"))?;
                overrides.mouse_sensitivity = Some(
                    v.parse::<u16>()
                        .map_err(|_| anyhow!("--mouse-sensitivity must be a number 0-100"))?,
                );
            }
            "--mouse-capture" => {
                let v = args.next().ok_or_else(|| {
                    anyhow!("--mouse-capture requires a mode (click, auto, or manual)")
                })?;
                overrides.mouse_capture = Some(v);
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
                let port = take_port_token(&mut args, 1);
                click_after.push((secs, button, dur_ms, port));
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
                let port = take_port_token(&mut args, 2);
                joy_after.push((secs, button, dur_ms, port));
            }
            "--mouse-to-after" => {
                const USAGE: &str = "--mouse-to-after requires SECS X Y";
                let secs: f32 =
                    next_arg(&mut args, USAGE, "--mouse-to-after SECS must be a number")?;
                let x: i32 = next_arg(&mut args, USAGE, "--mouse-to-after X must be an integer")?;
                let y: i32 = next_arg(&mut args, USAGE, "--mouse-to-after Y must be an integer")?;
                let port = take_port_token(&mut args, 1);
                mouse_to_after.push((secs, x, y, port));
            }
            "--mouse-after" => {
                const USAGE: &str = "--mouse-after requires SECS DX DY";
                let secs: f32 = next_arg(&mut args, USAGE, "--mouse-after SECS must be a number")?;
                let dx: i32 = next_arg(&mut args, USAGE, "--mouse-after DX must be an integer")?;
                let dy: i32 = next_arg(&mut args, USAGE, "--mouse-after DY must be an integer")?;
                let port = take_port_token(&mut args, 1);
                mouse_after.push((secs, dx, dy, port));
            }
            "--pot-after" => {
                const USAGE: &str = "--pot-after requires SECS X Y";
                let secs: f32 = next_arg(&mut args, USAGE, "--pot-after SECS must be a number")?;
                let x: u8 = next_arg(&mut args, USAGE, "--pot-after X must be a number 0-255")?;
                let y: u8 = next_arg(&mut args, USAGE, "--pot-after Y must be a number 0-255")?;
                let port = take_port_token(&mut args, 2);
                pot_after.push((secs, x, y, port));
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
            "--insert-cd-after" => {
                const USAGE: &str = "--insert-cd-after requires SECS PATH";
                let secs: f32 =
                    next_arg(&mut args, USAGE, "--insert-cd-after SECS must be a number")?;
                let path = args.next().ok_or_else(|| anyhow!(USAGE))?;
                cd_insert_after.push((secs, PathBuf::from(path)));
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
            "--control" => {
                let listen = args
                    .next()
                    .ok_or_else(|| anyhow!("--control requires ADDR, :PORT, or PORT"))?;
                control_listen = Some(listen);
            }
            "--control-gui" => {
                let listen = args
                    .next()
                    .ok_or_else(|| anyhow!("--control-gui requires ADDR, :PORT, or PORT"))?;
                control_gui_listen = Some(listen);
            }
            "--control-token" => {
                let token = args
                    .next()
                    .ok_or_else(|| anyhow!("--control-token requires a token string"))?;
                control_token = Some(token);
            }
            "--control-info" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow!("--control-info requires a file path"))?;
                control_info = Some(PathBuf::from(path));
            }
            "--dump-frames" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow!("--dump-frames requires a directory"))?;
                dump_dir = Some(PathBuf::from(path));
            }
            "--waveform" => {
                let path = args
                    .next()
                    .ok_or_else(|| anyhow!("--waveform requires a VCD output path"))?;
                wave_path = Some(PathBuf::from(path));
            }
            "--wave-trigger" => {
                const USAGE: &str =
                    "--wave-trigger SPEC: now, pc=ADDR, beam=VPOS[:HPOS], reg=OFF, or time=SECS";
                let spec = args.next().ok_or_else(|| anyhow!(USAGE))?;
                wave_trigger = Some(
                    copperline::waveform::parse_trigger(&spec)
                        .ok_or_else(|| anyhow!("bad trigger {spec:?}; {USAGE}"))?,
                );
            }
            "--wave-duration" => {
                const USAGE: &str =
                    "--wave-duration SPEC: Ncck (bare N is cck), Nf/Nframes, Nms, or Ns";
                let spec = args.next().ok_or_else(|| anyhow!(USAGE))?;
                wave_duration = Some(
                    copperline::waveform::parse_duration(&spec)
                        .ok_or_else(|| anyhow!("bad duration {spec:?}; {USAGE}"))?,
                );
            }
            "--wave-signals" => {
                const USAGE: &str = "--wave-signals LIST: comma list of \
                     beam, bus, cpu, copper, blitter, regs, irq, audio, or all";
                let spec = args.next().ok_or_else(|| anyhow!(USAGE))?;
                wave_signals = Some(
                    copperline::waveform::parse_signals(&spec)
                        .ok_or_else(|| anyhow!("bad signal list {spec:?}; {USAGE}"))?,
                );
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
    let waveform = match wave_path {
        Some(path) => {
            let mut opts = copperline::waveform::WaveOptions::new(path);
            if let Some(trigger) = wave_trigger {
                opts.trigger = trigger;
            }
            if let Some(duration) = wave_duration {
                opts.duration = duration;
            }
            if let Some(signals) = wave_signals {
                opts.signals = signals;
            }
            Some(opts)
        }
        None => {
            if wave_trigger.is_some() || wave_duration.is_some() || wave_signals.is_some() {
                return Err(anyhow!(
                    "--wave-trigger/--wave-duration/--wave-signals require --waveform PATH"
                ));
            }
            None
        }
    };
    if control_listen.is_some() && control_gui_listen.is_some() {
        return Err(anyhow!("--control and --control-gui cannot be combined"));
    }
    if control_listen.is_none()
        && control_gui_listen.is_none()
        && (control_token.is_some() || control_info.is_some())
    {
        return Err(anyhow!(
            "--control-token/--control-info require --control or --control-gui"
        ));
    }
    if overrides.a2065_interface.is_some()
        && overrides
            .a2065_net
            .as_deref()
            .is_some_and(|net| !matches!(net.to_ascii_lowercase().as_str(), "bridge" | "bridged"))
    {
        return Err(anyhow!(
            "--a2065-interface conflicts with an explicit non-bridge --a2065-net"
        ));
    }
    Ok(CliArgs {
        config_path,
        rom_path,
        screenshot_after,
        save_state_after,
        load_state,
        benchmark_until,
        gdb,
        control: control_listen,
        control_gui: control_gui_listen,
        control_token,
        control_info,
        frame_dump,
        waveform,
        press_after,
        click_after,
        joy_after,
        mouse_after,
        mouse_to_after,
        pot_after,
        record_input,
        disk_insert_after,
        cd_insert_after,
        audio_live,
        audio_live_forced: explicit_audio_live,
        audio_wav,
        live_audio_profile_secs,
        calibrate_gamepad,
        list_midi,
        list_audio_devices,
        list_net_interfaces,
        net_helper_action,
        list_sampler_inputs,
        overrides,
    })
}

fn print_help() {
    let shortcut = HOST_SHORTCUT_MODIFIER_LABEL;
    // The MIDI endpoint options only do anything in a `midi`-feature build, so
    // list them only there. `--serial` itself is always shown: off/stdout work
    // in every build, and it names midi as a mode. The MT-32 rides with them
    // when its own feature is in, being reached through `--midi-out`.
    #[cfg(all(feature = "midi", feature = "mt32"))]
    let midi = "--midi-out NAME                host MIDI destination, or mt32 (implies --serial midi)\n  \
                --midi-in NAME                 host MIDI source, or mt32 (implies --serial midi)\n  \
                --list-midi                    list host MIDI endpoints and exit\n  \
                --mt32-control-rom PATH        control ROM for the emulated MT-32\n  \
                --mt32-pcm-rom PATH            PCM ROM for the emulated MT-32\n  \
                --mt32-panel                   show the MT-32's front panel under the display\n  ";
    #[cfg(all(feature = "midi", not(feature = "mt32")))]
    let midi = "--midi-out NAME                host MIDI destination (implies --serial midi)\n  \
                --midi-in NAME                 host MIDI source (implies --serial midi)\n  \
                --list-midi                    list host MIDI endpoints and exit\n  ";
    #[cfg(not(feature = "midi"))]
    let midi = "";
    // A build without the feature cannot attach a physical drive at all, so
    // the flags are not listed and not accepted.
    #[cfg(feature = "floppybridge")]
    let floppy_bridge =
        "--floppy-bridge DFN NAME       drive a physical floppy drive on DFN over NAME:\n  \
         \x20                            drawbridge, greaseweazle, supercardpro, or off\n  \
         --floppy-bridge-port DFN PORT  serial port of that interface (default: auto-detect)\n  \
         --floppy-bridge-cable DFN SEL  drive select on the cable: a/b (IBM PC) or 0-3 (Shugart)\n  \
         --floppy-bridge-mode DFN MODE  how tracks are captured: normal, compatible, stalling\n  \
         --floppy-bridge-density DFN D  force a density: auto, dd, or hd\n  \
         --floppy-bridge-speed DFN PCT  serve captured tracks at 100, 125, 150, 175, or 200%\n  \
         --floppy-bridge-auto-cache DFN   cache disk data while the drive is idle\n  \
         --floppy-bridge-writable DFN   let the guest write to the physical disk (which is\n  \
         \x20                            write-protected unless asked otherwise)\n  ";
    #[cfg(not(feature = "floppybridge"))]
    let floppy_bridge = "";
    eprintln!(
        "copperline - Amiga emulator\n\
         \n\
         Usage: copperline [--config FILE] [--screenshot-after SECS PATH] [ROM]\n\
         \n\
         Options:\n  \
         -c, --config FILE              load configuration from FILE (default: ./copperline.toml)\n  \
         --model NAME                   machine profile: A1000, A500, A500OCS, A500Plus, A600,\n  \
         \x20                              A1200, A3000, A4000, CDTV, CD32\n  \
         --chipset NAME                 chipset preset: OCS, ECS, or AGA\n  \
         --cpu MODEL                    CPU: 68000, 68010, 68EC020, 68020, 68030, 68040, or 68060\n  \
         --cpu-clock MHZ                CPU clock in MHz (default: the model's stock speed)\n  \
         --fpu / --no-fpu               fit / omit a 68881/68882 (68040/68060 on-die)\n  \
         --jit / --no-jit               fast batch/trace-JIT CPU execution (not cycle-exact,\n  \
         \x20                            like an accelerator card; default: off)\n  \
         --chip SIZE                    chip RAM size, e.g. 512K, 1M, 2M\n  \
         --fast SIZE                    Zorro II fast RAM size, e.g. 0, 1M, 4M, 8M\n  \
         --slow SIZE                    trapdoor slow RAM at $C00000, e.g. 0, 512K\n  \
         --motherboard SIZE             Ramsey motherboard fast RAM (A3000/A4000), e.g. 0, 4M,\n  \
         \x20                            16M; the A4000 extends to 64M\n  \
         --accelerator SIZE             CPU-slot accelerator fast RAM at $08000000 (32-bit\n  \
         \x20                            CPUs), e.g. 0, 32M, 128M\n  \
         --floppy-drives COUNT          wired floppy drives, 1-4 (DF0 plus externals)\n  \
         --floppy-speed PERCENT         drive speed: 100, 200, 400, 800, or 0 (turbo)\n  \
         {floppy_bridge}--rtc-time TIME                seed the battery clock (implies fitting one) with\n  \
         \x20                            Unix seconds or \"YYYY-MM-DD HH:MM[:SS]\"; it then\n  \
         \x20                            ticks in emulated time, so runs are deterministic\n  \
         --rtc-frozen                   stop the seeded clock at --rtc-time exactly\n  \
         --joystick MODE                initial joystick input: gamepad or keyboard\n  \
         \x20                            (gamepad lets the keyboard pass through to the Amiga)\n  \
         --mouse-sensitivity N          host mouse sensitivity 0-100 (50 default = 1:1)\n  \
         --mouse-capture MODE           when to grab the host mouse: click (default), auto, manual\n  \
         --port1 DEVICE                 controller in port 1: mouse (default), joystick,\n  \
         \x20                            cd32, analogue, or none\n  \
         --port2 DEVICE                 controller in port 2 (default: joystick;\n  \
         \x20                            cd32 on the CD32 profile)\n  \
         --autofire HZ                  pulse a held fire button at HZ (0 = off, the default)\n  \
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
         --control ADDR                 run the headless JSON-RPC control server on ADDR\n  \
         \x20                            (port 0 picks a free port; see docs/debugger/control.md)\n  \
         --control-gui ADDR             attach the control server to the normal window\n  \
         --control-token TOKEN          pin the control auth token (default: generated;\n  \
         \x20                            visible in ps -- prefer --control-info)\n  \
         --control-info PATH            write the control endpoint and token to PATH as JSON\n  \
         --dump-frames DIR              dump consecutive PNG frames into DIR, then exit\n  \
         --dump-start SECS              start frame dumping after SECS seconds (default: 0)\n  \
         --dump-count COUNT             number of frames to dump with --dump-frames\n  \
         --waveform PATH                arm a VCD logic-analyser capture of chipset signals\n  \
         \x20                            for GTKWave (see docs/debugger/waveform.md)\n  \
         --wave-trigger SPEC            capture trigger: now (default), pc=ADDR,\n  \
         \x20                            beam=VPOS[:HPOS], reg=OFF, or time=SECS\n  \
         --wave-duration SPEC           capture length: Ncck (bare N is cck), Nf,\n  \
         \x20                            Nms, or Ns (default: 1 frame)\n  \
         --wave-signals LIST            comma list of beam, bus, cpu, copper, blitter,\n  \
         \x20                            regs, irq, audio (default: all)\n  \
         --press-after SECS KEY         press/release Amiga KEY after SECS; KEY may be\n  \
         \x20                            decimal, 0x.., or a name like ctrl/lalt/lami/f1\n  \
         --key-after SECS KEY MS        press KEY after SECS, hold for MS milliseconds,\n  \
         \x20                            then release; may be passed multiple times\n  \
         --click-after SECS BTN MS [PORT]\n  \
         \x20                            press mouse BTN (left/right/middle) at SECS,\n  \
         \x20                            release MS ms later, on PORT (default 1)\n  \
         --joy-after SECS BTN MS [PORT] press joystick/CD32-pad BTN (up/down/left/right/\n  \
         \x20                            red|fire/blue/green/yellow/play/rwd/ffw) at SECS,\n  \
         \x20                            release MS ms later, on PORT (default 2)\n  \
         --mouse-after SECS DX DY [PORT]\n  \
         \x20                            apply a relative mouse motion at SECS on PORT\n  \
         \x20                            (default 1)\n  \
         --mouse-to-after SECS X Y [PORT]\n  \
         \x20                            from SECS, move the pointer to screen pixel\n  \
         \x20                            (X, Y) by watching sprite 0, on PORT (default 1)\n  \
         --pot-after SECS X Y [PORT]    set an analogue controller position (0-255 per\n  \
         \x20                            axis) at SECS on PORT (default 2)\n  \
         --record-input PATH            record all machine-bound input for the whole run\n  \
         \x20                            and write the script to PATH on exit\n  \
         --script FILE                  run scripted-input directives from FILE (the flag\n  \
         \x20                            syntax without the dashes; # comments allowed);\n  \
         \x20                            {shortcut}+Shift+R records a live session into this format\n  \
         --insert-disk-after SECS DFN PATH\n  \
         \x20                            insert PATH into DFN after SECS seconds\n  \
         --defer-disk-insert SECS DFN   start with configured DFN empty, then insert\n  \
         \x20                            its configured disk image after SECS seconds\n  \
         --insert-cd-after SECS PATH    swap the CD image (cue/iso/chd) in the machine's CD\n  \
         \x20                            drive (CDTV, CD32, or a SCSI CD-ROM unit) after\n  \
         \x20                            SECS seconds\n  \
         --audio                        enable real-time stereo audio output via cpal (default)\n  \
         --noaudio                      disable real-time audio output\n  \
         --audio-device NAME            host audio output device (substring match)\n  \
         --audio-channel-mode MODE      output channels: stereo (default) or mono\n  \
         --audio-filter MODE            Paula filter: auto (default), on, or off\n  \
         --audio-stereo-separation PCT  stereo width 0-100 (100 default, 0 = mono)\n  \
         --list-audio-devices           list host audio output devices and exit\n  \
         --list-net-interfaces          list adapters usable for bridged Ethernet and exit\n  \
         --install-net-helper           install Linux bridge helper (CAP_NET_RAW only)\n  \
         --uninstall-net-helper         remove the Linux bridge helper\n  \
         --net-helper-status            report Linux bridge-helper status\n  \
         --audio-wav PATH               dump mixed stereo audio to a 32-bit float WAV file\n  \
         \x20                            instead of live output\n  \
         --profile-live-audio SECS      run a no-window Paula-to-cpal profile workload;\n  \
         \x20                            combine with COPPERLINE_AUDIO_PROFILE=1 for counters\n  \
         --full-screen / --windowed     open fullscreen / windowed at start (default: windowed)\n  \
         --show-status-bar / --hide-status-bar  status bar at start (default: shown)\n  \
         --perf-overlay                 show the performance overlay at start\n  \
         \x20                            (Cmd/Alt+P toggles it live)\n  \
         --menu-scale SIZE              size of the pop-up menu: 1x (default) or 2x\n  \
         --mt32-control-rom PATH        Munt MT-32 control ROM (with --mt32-pcm-rom,\n  \
         \x20                            makes \"mt32\" selectable as the MIDI output)\n  \
         --mt32-pcm-rom PATH            Munt MT-32 PCM ROM\n  \
         --mt32-panel                   show the Munt MT-32 front panel\n  \
         --serial MODE                  Paula serial port: off, stdout, midi, tcp,\n  \
         \x20                            tcp-connect, or pty\n  \
         --serial-connect HOST:PORT     dial a remote TCP service (a telnet BBS) with the\n  \
         \x20                            serial port (implies --serial tcp-connect)\n  \
         --a2065-net BACKEND            fit an A2065 Ethernet board: none, loopback, nat,\n  \
         \x20                            or bridge (direct attachment to a host adapter)\n  \
         --a2065-interface NAME         bridge adapter; implies --a2065-net bridge\n  \
         --parallel DEVICE              parallel port: none, printer, or sampler\n  \
         --sampler-audio-input NAME     sampler host capture device (implies --parallel sampler)\n  \
         --sampler-input-gain DB        sampler input gain in dB (implies --parallel sampler)\n  \
         --sampler-list-audio-inputs    list host audio input devices and exit\n  \
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
         CPU: 68000   chip RAM: 512K   slow RAM: 512K   fast RAM: 0   chipset: ECS\n  \
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

fn parse_floppy_speed(s: &str) -> Result<u16> {
    const MSG: &str = "--floppy-speed PERCENT must be 100, 200, 400, 800, or 0 (turbo)";
    let speed: u16 = s.trim().parse().map_err(|_| anyhow!(MSG))?;
    if speed != copperline::floppy::SPEED_TURBO
        && !copperline::floppy::SUPPORTED_SPEED_PERCENTS.contains(&speed)
    {
        return Err(anyhow!(MSG));
    }
    Ok(speed)
}

#[cfg(feature = "floppybridge")]
fn parse_floppy_bridge_speed(s: &str) -> Result<u16> {
    const MSG: &str = "--floppy-bridge-speed PERCENT must be 100, 125, 150, 175, or 200";
    let speed: u16 = s.trim().parse().map_err(|_| anyhow!(MSG))?;
    if !copperline::config::SUPPORTED_BRIDGE_SPEED_PERCENTS.contains(&speed) {
        return Err(anyhow!(MSG));
    }
    Ok(speed)
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
        || !cli.mouse_to_after.is_empty()
        || !cli.pot_after.is_empty()
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
        || !cli.mouse_to_after.is_empty()
        || !cli.pot_after.is_empty()
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

fn validate_control_args(cli: &CliArgs) -> Result<()> {
    #[cfg(not(feature = "control"))]
    if cli.control.is_some() || cli.control_gui.is_some() {
        return Err(anyhow!(
            "this build was compiled without the control feature; \
             rebuild with --features control for --control/--control-gui"
        ));
    }
    if cli.control.is_some() || cli.control_gui.is_some() {
        if cli.gdb.is_some() {
            return Err(anyhow!(
                "--control/--control-gui cannot be combined with --gdb"
            ));
        }
        if cli.benchmark_until.is_some() {
            return Err(anyhow!(
                "--control/--control-gui cannot be combined with --benchmark-until"
            ));
        }
    }
    if cli.control.is_none() {
        return Ok(());
    }
    // The headless server owns the machine like --gdb does; the windowed
    // App (which fires the scheduled/capture flags) never runs. Input
    // recording IS supported: the server journals injected input itself.
    if cli.screenshot_after.is_some() {
        return Err(anyhow!(
            "--control cannot be combined with --screenshot-after (use capture.screenshot)"
        ));
    }
    if cli.save_state_after.is_some() {
        return Err(anyhow!(
            "--control cannot be combined with --save-state-after (use state.save)"
        ));
    }
    if cli.frame_dump.is_some() {
        return Err(anyhow!("--control cannot be combined with --dump-frames"));
    }
    if cli.live_audio_profile_secs.is_some() {
        return Err(anyhow!(
            "--control cannot be combined with --profile-live-audio"
        ));
    }
    if !cli.press_after.is_empty()
        || !cli.click_after.is_empty()
        || !cli.joy_after.is_empty()
        || !cli.mouse_after.is_empty()
        || !cli.mouse_to_after.is_empty()
        || !cli.pot_after.is_empty()
    {
        return Err(anyhow!(
            "--control cannot be combined with scheduled input events (use input.*)"
        ));
    }
    if !cli.disk_insert_after.is_empty() {
        return Err(anyhow!(
            "--control cannot be combined with scheduled disk inserts (use media.*)"
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
    let mut frame_times: Vec<f64> = Vec::new();
    while emu.bus().emulated_seconds() < target_secs {
        let frame_started = Instant::now();
        emu.step_frame()?;
        frame_times.push(frame_started.elapsed().as_secs_f64() * 1_000.0);
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
    report_benchmark_frame_times(start_frames, &frame_times);
    emu.report_stats();
    // Evaluate an untargeted reverse watchpoint at the benchmark's end.
    emu.tt_finalize_reverse_watch()?;
    Ok(())
}

/// A frame slower than this stalls the audio ring on a PAL host (50 Hz = 20 ms
/// per frame, minus headroom for the window render path).
const BENCH_FRAME_BUDGET_MS: f64 = 20.0;

/// Summarize the per-frame wall times of a `--benchmark-until` run: the
/// distribution, and every frame that individually blew the audio budget.
/// Averages hide these spikes, and a single late frame is an audible underrun.
fn report_benchmark_frame_times(start_frame: u64, frame_times: &[f64]) {
    if frame_times.is_empty() {
        return;
    }
    let mut sorted: Vec<f64> = frame_times.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
    info!(
        "benchmark frame times: p50={:.2}ms p90={:.2}ms p99={:.2}ms max={:.2}ms",
        pct(0.50),
        pct(0.90),
        pct(0.99),
        sorted[sorted.len() - 1]
    );
    let over: Vec<(usize, f64)> = frame_times
        .iter()
        .copied()
        .enumerate()
        .filter(|&(_, ms)| ms > BENCH_FRAME_BUDGET_MS)
        .collect();
    if over.is_empty() {
        info!(
            "benchmark frame times: all {} frames within the {:.0}ms budget",
            frame_times.len(),
            BENCH_FRAME_BUDGET_MS
        );
        return;
    }
    info!(
        "benchmark frame times: {} of {} frames over the {:.0}ms budget:",
        over.len(),
        frame_times.len(),
        BENCH_FRAME_BUDGET_MS
    );
    for (idx, ms) in over.iter().take(50) {
        info!("  frame {} ({:.2}ms)", start_frame + *idx as u64, ms);
    }
    if over.len() > 50 {
        info!("  ... and {} more", over.len() - 50);
    }
}

/// Whether to open a live audio sink. `[audio] output_enabled = false` (the GUI
/// "Disabled" option) silences default-on audio, but an explicit `--audio`
/// (`forced_on`) overrides it. `--noaudio` (which clears `audio_live`) and
/// `--audio-wav` still win; those are handled by the caller.
fn live_audio_enabled(audio_live: bool, forced_on: bool, config_enabled: bool) -> bool {
    audio_live && (forced_on || config_enabled)
}

/// Print the host audio output devices for `--list-audio-devices`. These are the
/// names `--audio-device` and `[audio] output_device` match against.
fn print_audio_output_devices() -> Result<()> {
    println!("Audio output devices (for --audio-device / [audio] output_device):");
    let devices = copperline::audio::list_output_devices();
    if devices.is_empty() {
        println!("  (none found)");
    }
    for name in devices {
        println!("  {name}");
    }
    Ok(())
}

/// Print exact adapter identifiers accepted by bridged networking.
fn print_net_interfaces() -> Result<()> {
    #[cfg(all(feature = "net-bridge", not(target_arch = "wasm32")))]
    {
        println!("Network interfaces (for --a2065-interface / [a2065] interface):");
        let interfaces = copperline::net::bridge::list_interfaces()?;
        if interfaces.is_empty() {
            println!("  (none found)");
        }
        for interface in interfaces {
            let mut state = Vec::new();
            if interface.up {
                state.push("up");
            } else {
                state.push("down");
            }
            if interface.running {
                state.push("running");
            }
            if interface.loopback {
                state.push("loopback");
            }
            if interface.wireless {
                state.push("wireless; bridging is best-effort");
            }
            println!("  {}\t[{}]", interface.label(), state.join(", "));
        }
        Ok(())
    }
    #[cfg(not(all(feature = "net-bridge", not(target_arch = "wasm32"))))]
    {
        anyhow::bail!("this build has no native bridged-networking support")
    }
}

fn run_net_helper_setup(action: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("FLATPAK_ID").is_some() {
            if action == "status" {
                #[cfg(feature = "net-bridge")]
                {
                    let socket = copperline::net::bridge::linux::helper_socket_path()?;
                    if socket.exists() {
                        println!("host helper socket is visible at {}", socket.display());
                        return Ok(());
                    }
                    anyhow::bail!(
                        "host helper socket is not visible at {}; install and enable \
                         the Linux network-helper companion on the host",
                        socket.display()
                    );
                }
                #[cfg(not(feature = "net-bridge"))]
                anyhow::bail!("this build has no bridged-networking support");
            }
            anyhow::bail!(
                "the Flatpak cannot install a host capability binary from \
                 inside its sandbox; download the Copperline Linux network-helper \
                 companion archive, then run its copperline-net-helper-setup {action}"
            );
        }
        let executable = std::env::current_exe()?;
        let mut candidates = vec![
            executable.with_file_name("copperline-net-helper-setup"),
            executable
                .parent()
                .and_then(Path::parent)
                .map(|prefix| {
                    prefix
                        .join("libexec")
                        .join("copperline")
                        .join("copperline-net-helper-setup")
                })
                .unwrap_or_default(),
            PathBuf::from("packaging/linux/copperline-net-helper-setup"),
        ];
        if let Some(appdir) = std::env::var_os("APPDIR") {
            candidates.insert(
                0,
                PathBuf::from(appdir).join("usr/libexec/copperline/copperline-net-helper-setup"),
            );
        }
        let setup = candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "copperline-net-helper-setup was not found; use the Linux \
                     network-helper companion archive from the Copperline release"
                )
            })?;
        let status = std::process::Command::new(&setup).arg(action).status()?;
        if !status.success() {
            anyhow::bail!("{} {action} failed with {status}", setup.display());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = action;
        anyhow::bail!("the capability helper is only used for bridged networking on Linux")
    }
}

/// Print the host audio input devices for `--sampler-list-audio-inputs`. These
/// are the names `--sampler-audio-input` and `[parallel] sampler_input` match
/// against.
fn print_sampler_input_devices() -> Result<()> {
    println!("Audio input devices (for --sampler-audio-input / [parallel] sampler_input):");
    let devices = copperline::sampler::list_input_devices();
    if devices.is_empty() {
        println!("  (none found)");
    }
    for name in devices {
        println!("  {name}");
    }
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
        // The Cranelift JIT backend logs every compiled trace's full IR
        // listing at info level; with `[cpu] jit` that floods the log.
        // RUST_LOG still opts back in for JIT debugging.
        log_builder.filter_module("cranelift_jit", log::LevelFilter::Warn);
        log_builder.filter_module("cranelift_codegen", log::LevelFilter::Warn);
    }
    log_builder.init();

    crashlog::install();

    let cli = parse_args()?;
    validate_benchmark_args(&cli)?;
    validate_gdb_args(&cli)?;
    validate_control_args(&cli)?;
    if cli.calibrate_gamepad {
        return gamepad::run_calibration();
    }
    if cli.list_midi {
        return list_midi_endpoints();
    }
    if cli.list_audio_devices {
        return print_audio_output_devices();
    }
    if cli.list_net_interfaces {
        return print_net_interfaces();
    }
    if let Some(action) = cli.net_helper_action.as_deref() {
        return run_net_helper_setup(action);
    }
    if cli.list_sampler_inputs {
        return print_sampler_input_devices();
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
    // `[audio] output_enabled = false` (the GUI "Disabled" option) silences
    // default-on audio, but an explicit `--audio` still forces it on and
    // `--noaudio`/`--audio-wav` still win. CLI flags are unchanged.
    let live_audio = live_audio_enabled(
        cli.audio_live,
        cli.audio_live_forced,
        cfg.audio.output_enabled,
    );
    let audio: Box<dyn AudioSink> = if let Some(ref wav_path) = cli.audio_wav {
        Box::new(WavSink::new(wav_path)?)
    } else if live_audio {
        Box::new(CpalSink::new(
            realtime_priority,
            cfg.audio.output_device.as_deref(),
        )?)
    } else {
        // Log the silent path so `--noaudio` (or an output_enabled=false config)
        // is visible alongside the "cpal sink ready" line the live path prints.
        info!("audio: disabled (null sink); no sound");
        Box::new(NullSink)
    };
    // Headless capture runs (screenshot / frame dump) advance the
    // deterministic core unthrottled; the interactive window paces to
    // wall-clock time. The emulated result is identical either way.
    let headless_capture = cli.screenshot_after.is_some()
        || cli.frame_dump.is_some()
        || cli.benchmark_until.is_some()
        || cli.gdb.is_some()
        || cli.control.is_some();
    // A real drive on a bridge is the exception: its platter turns in
    // wall-clock time and cannot be hurried. Left unthrottled, the emulated
    // machine outruns it -- spinning the motor up and down faster than it can
    // reach speed, and stepping past tracks before the drive has captured
    // them -- so the guest sees a drive that answers almost nothing. Pace a
    // bridged machine like a real Amiga, whatever else was asked for.
    let bridged = cfg.floppy.bridges.iter().any(Option::is_some);
    let paced = !headless_capture || bridged;
    if bridged && headless_capture {
        info!("emulation timing: paced to wall-clock because a physical floppy drive is attached");
    }
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
    if let Some(opts) = cli.waveform {
        emu.machine.ui_wave_start(opts)?;
    }
    if let Some(target_secs) = cli.benchmark_until {
        return run_headless_benchmark(emu, target_secs);
    }
    if let Some(gdb) = cli.gdb {
        return gdbstub::run(emu, gdb);
    }
    #[cfg(feature = "control")]
    if let Some(listen) = cli.control.clone() {
        let mut config = copperline::control::Config::new(listen);
        config.token = cli.control_token.clone();
        config.info_file = cli.control_info.clone();
        // The headless server owns the machine, so it journals
        // --record-input itself; windowed mode journals through the App.
        config.record_input = cli.record_input.clone();
        return copperline::control::headless::run(emu, config);
    }
    let disk_write_protected = std::array::from_fn(|idx| {
        cfg.floppy.drives[idx]
            .as_ref()
            .map(|d| d.write_protected)
            .unwrap_or(true)
    });
    video::set_pixel_aspect(config::resolve_pixel_aspect(cfg.pixel_aspect));
    video::set_display_scaling(cfg.scaling);
    video::set_menu_scale(cfg.menu_scale);
    video::set_mt32_panel_shown(cfg.serial.mt32_panel);
    video::set_mt32_lcd(cfg.serial.mt32_lcd);
    // Capture runs (--screenshot-after / --dump-frames) never present a
    // frame, so they skip the host window and event loop entirely: winit's
    // event-loop setup registers with the display server, which aborts or
    // blocks on hosts without one (SSH sessions, sandboxes without
    // window-server access), and a capture run must work anywhere.
    // --control-gui keeps the windowed path: it explicitly asks for an
    // interactive session.
    let windowless_capture =
        (cli.screenshot_after.is_some() || cli.frame_dump.is_some()) && cli.control_gui.is_none();
    #[cfg_attr(not(feature = "control"), allow(unused_mut))]
    let mut app = App::new(
        emu,
        cfg.emulation.power_on,
        cli.screenshot_after,
        cli.save_state_after,
        cli.frame_dump,
        cli.press_after,
        cli.click_after,
        cli.joy_after,
        cli.mouse_after,
        cli.mouse_to_after,
        cli.pot_after,
        disk_insert_after,
        cli.cd_insert_after,
        cli.record_input,
        cfg.floppy_playlists.clone(),
        disk_write_protected,
        config::resolve_overscan(cfg.overscan),
        config::resolve_deinterlace(cfg.deinterlace),
        config::resolve_phosphor(cfg.phosphor),
        config::resolve_shader(cfg.shader.clone()),
        config::resolve_shader_strength(cfg.shader_strength),
        config::resolve_bezel(cfg.bezel),
        config::resolve_perf_overlay(cfg.perf_overlay),
        config::resolve_tint(cfg.tint),
        cfg.full_screen,
        !cfg.status_bar,
        cfg.emulation.warp_speed,
        cfg.joystick_input_mode,
        cfg.mouse_sensitivity,
        cfg.mouse_capture,
        config::about_machine_lines(&cfg),
        raw_cfg,
        live_audio,
        copperline::sampler::SamplerRequest::from_config(&cfg.parallel),
    );
    #[cfg(feature = "control")]
    if let Some(listen) = cli.control_gui {
        // Bind (and announce) before the window opens so scripts can
        // attach as soon as the endpoint line appears; the socket
        // threads start inside App::run once the event loop exists.
        let mut config = copperline::control::Config::new(listen);
        config.token = cli.control_token;
        config.info_file = cli.control_info;
        let handle = copperline::control::windowed::ControlHandle::bind(&config)?;
        app.attach_control(handle, &config);
    }

    // Elevate the thread that is about to run the event loop and the pacer.
    // Only when actually pacing to wall-clock time: headless capture advances
    // the core unthrottled, so priority buys it nothing.
    if realtime_priority && paced {
        priority::elevate_pacer_thread();
    }
    if windowless_capture {
        info!("headless capture: running without a window (no display connection)");
        return app.run_headless();
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
        mb_ram: Vec::new(),
        accel_ram: Vec::new(),
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
    // The launcher opens before a machine config is built, so it presents at
    // the default aspect and scaling; the machine it starts applies its own
    // (see start_configured_machine).
    video::set_display_scaling(config::DisplayScaling::Smooth);
    // The launcher opens before a machine config is built, so the menu size
    // comes straight off the raw file.
    video::set_menu_scale(raw_cfg.menu_scale());
    // The placeholder is always silent; seed the session's audio from the config
    // intent so a state loaded over the launcher gets the configured output.
    let audio_output_enabled = raw_cfg.audio_output_enabled();
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
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        std::array::from_fn(|_| Vec::new()),
        [true; 4],
        config::resolve_overscan(config::Overscan::Tv),
        config::resolve_deinterlace(true),
        config::resolve_phosphor(0.0),
        config::resolve_shader(config::ShaderMode::None),
        config::resolve_shader_strength(1.0),
        config::resolve_bezel(false),
        config::resolve_perf_overlay(false),
        config::resolve_tint(config::Tint::None),
        // The config-screen placeholder is always a normal windowed UI.
        false,
        false,
        config::WarpSpeed::default(),
        config::JoystickInputMode::default(),
        50,
        // The config screen is a UI to be clicked around; an auto grab
        // belongs to the machine, and run_machine installs the real setting
        // when one is started.
        config::MouseCapture::default(),
        vec!["Configure a machine, then press Run.".to_string()],
        raw_cfg,
        audio_output_enabled,
        // The placeholder runs no sampler; run_machine attaches it on Run.
        copperline::sampler::SamplerRequest::default(),
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
        && cli.control.is_none()
        && cli.control_gui.is_none()
        && cli.load_state.is_none()
        && cli.press_after.is_empty()
        && cli.click_after.is_empty()
        && cli.joy_after.is_empty()
        && cli.mouse_after.is_empty()
        && cli.mouse_to_after.is_empty()
        && cli.pot_after.is_empty()
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
    // This diagnostic mode loads no config, so the realtime knob is env-only
    // and it always uses the default output device.
    let audio = Box::new(CpalSink::new(priority::requested(false), None)?);
    let mut paula = Paula::new(Box::new(StdoutSink::new()), audio);
    paula.set_led_filter_guest(true);

    let mut chip_ram = vec![0u8; 64];
    chip_ram[0] = 0x40;
    chip_ram[1] = 0xC0;
    chip_ram[2] = 0x20;
    chip_ram[3] = 0xE0;

    paula.write_audio_reg(0x00, 0, 0);
    paula.write_audio_reg(0x02, 0, 0);
    paula.write_audio_reg(0x04, 1, 0);
    paula.write_audio_reg(0x06, 400, 0);
    paula.write_audio_reg(0x08, 64, 0);
    paula.write_audio_reg(0x10, 0, 0);
    paula.write_audio_reg(0x12, 2, 0);
    paula.write_audio_reg(0x14, 1, 0);
    paula.write_audio_reg(0x16, 512, 0);
    paula.write_audio_reg(0x18, 48, 0);

    let dmacon = DMACON_DMAEN | 0x0003;
    paula.apply_audio_dmacon_edges(0, dmacon);
    let mut line_cck = 0u32;
    let quantum = Duration::from_millis(5);
    let quantum_cck = (PAULA_CLOCK_HZ as f64 * quantum.as_secs_f64())
        .round()
        .clamp(1.0, u32::MAX as f64) as u32;
    let started = Instant::now();
    let deadline = started + Duration::from_secs_f32(secs);
    let mut chunks = 0u64;

    while Instant::now() < deadline {
        let chunk_started = Instant::now();
        let _ =
            advance_paula_profile_audio(&mut paula, quantum_cck, dmacon, &chip_ram, &mut line_cck);
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

fn advance_paula_profile_audio(
    paula: &mut Paula,
    cck: u32,
    dmacon: u16,
    chip_ram: &[u8],
    line_cck: &mut u32,
) -> u16 {
    // Drive the state machine the way the bus does: service each channel's
    // fixed DMA slot, advance time, and transfer requests at line ends.
    let mut irq = 0;
    for _ in 0..cck {
        let slot = match *line_cck {
            0x00F => Some(0),
            0x011 => Some(1),
            0x013 => Some(2),
            0x015 => Some(3),
            _ => None,
        };
        if let Some(channel) = slot {
            if let Some(request) = paula.audio_dma_request(channel) {
                let word = read_profile_audio_word(chip_ram, request.address);
                irq |= paula.grant_audio_dma(channel, word, dmacon);
            }
        }
        irq |= paula.advance_audio(1, dmacon);
        *line_cck += 1;
        if *line_cck >= 227 {
            *line_cck = 0;
            paula.transfer_audio_dma_requests();
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

    #[test]
    fn bridge_cli_interface_implies_bridge_and_rejects_conflicts() {
        let args = parse(&["--a2065-interface", "en-test"]).unwrap();
        assert_eq!(args.overrides.a2065_interface.as_deref(), Some("en-test"));
        assert!(args.overrides.a2065_net.is_none());

        let args = parse(&["--a2065-net", "bridge", "--a2065-interface", "en-test"]).unwrap();
        assert_eq!(args.overrides.a2065_net.as_deref(), Some("bridge"));

        let error = parse(&["--a2065-net", "nat", "--a2065-interface", "en-test"]).unwrap_err();
        assert!(error.to_string().contains("conflicts"), "{error:#}");
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
        assert_eq!(args.mouse_after, vec![(1.5, -3, 10, 0)]);
        Ok(())
    }

    #[test]
    fn mouse_to_after_parses_absolute_targets_on_either_port() -> Result<()> {
        let args = parse(&["--mouse-to-after", "3.0", "320", "128"])?;
        assert_eq!(args.mouse_to_after, vec![(3.0, 320, 128, 0)]);
        // The same directive inside a script, with the optional port.
        let path = temp_script("mouse-to", "mouse-to-after 4.5 100 40 2\n");
        let args = parse(&["--script", &path.display().to_string()])?;
        assert_eq!(args.mouse_to_after, vec![(4.5, 100, 40, 1)]);
        std::fs::remove_file(&path).ok();
        Ok(())
    }

    #[test]
    fn scripted_input_flags_take_an_optional_trailing_port() -> Result<()> {
        let args = parse(&[
            "--mouse-after",
            "1.5",
            "-3",
            "10",
            "2",
            "--click-after",
            "5",
            "left",
            "100",
            "2",
            "--joy-after",
            "60",
            "red",
            "300",
            "1",
            "--pot-after",
            "12",
            "50",
            "200",
            "1",
        ])?;
        assert_eq!(args.mouse_after, vec![(1.5, -3, 10, 1)]);
        assert_eq!(args.click_after, vec![(5.0, MouseButtonKind::Left, 100, 1)]);
        assert_eq!(args.joy_after, vec![(60.0, JoyButtonKind::Red, 300, 0)]);
        assert_eq!(args.pot_after, vec![(12.0, 50, 200, 0)]);
        Ok(())
    }

    #[test]
    fn port_token_lookahead_does_not_eat_a_following_flag() -> Result<()> {
        // No trailing port: the next flag must survive as a flag, and the
        // defaults are click/mouse -> port 1, joy/pot -> port 2 (0-based
        // 0/1 in the tuples).
        let args = parse(&[
            "--joy-after",
            "60",
            "red",
            "300",
            "--pot-after",
            "12",
            "50",
            "200",
            "--noaudio",
        ])?;
        assert_eq!(args.joy_after, vec![(60.0, JoyButtonKind::Red, 300, 1)]);
        assert_eq!(args.pot_after, vec![(12.0, 50, 200, 1)]);
        assert!(!args.audio_live);
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
             joy-after 60.0 red 300 1\n\
             mouse-after 1.020 -3 10 2\n\
             pot-after 12.0 50 200\n\
             insert-disk-after 30.0 df1 \"/tmp/with space.adf\"\n",
        );
        let args = parse(&["--script", path.to_str().unwrap()])?;
        assert_eq!(args.press_after.len(), 2);
        assert_eq!(args.press_after[0].hold_ms, 500);
        assert_eq!(args.click_after, vec![(5.0, MouseButtonKind::Left, 100, 0)]);
        assert_eq!(args.joy_after, vec![(60.0, JoyButtonKind::Red, 300, 0)]);
        assert_eq!(args.mouse_after, vec![(1.02, -3, 10, 1)]);
        assert_eq!(args.pot_after, vec![(12.0, 50, 200, 1)]);
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
        input.set_port_device(1, copperline::bus::PortDevice::Joystick);
        rec.observe(&input, 1.0);
        rec.record_key(0x45, true, 1.5);
        rec.record_key(0x45, false, 1.75);
        input.ports[0].counter_x = 5;
        input.ports[0].fire = true;
        rec.observe(&input, 2.0);
        input.ports[0].fire = false;
        rec.observe(&input, 2.5);
        rec.record_disk_insert(0, Path::new("/tmp/demo.adf"), 3.0);
        let path = temp_script("roundtrip", &rec.finish());

        let args = parse(&["--script", path.to_str().unwrap()])?;
        assert_eq!(args.press_after.len(), 1);
        assert_eq!(args.press_after[0].rawkey, 0x45);
        assert_eq!(args.press_after[0].hold_ms, 250);
        assert_eq!(args.mouse_after, vec![(2.0, 5, 0, 0)]);
        assert_eq!(args.click_after, vec![(2.0, MouseButtonKind::Left, 500, 0)]);
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
    fn explicit_audio_marks_forced() -> Result<()> {
        assert!(!parse(&[])?.audio_live_forced);
        assert!(parse(&["--audio"])?.audio_live_forced);
        assert!(!parse(&["--noaudio"])?.audio_live_forced);
        Ok(())
    }

    #[test]
    fn config_disable_silences_default_audio_but_not_explicit_audio() {
        // No CLI audio flag: the config's output_enabled decides.
        assert!(live_audio_enabled(true, false, true));
        assert!(!live_audio_enabled(true, false, false));
        // Explicit --audio forces sound on even if the config disabled it.
        assert!(live_audio_enabled(true, true, false));
        // --noaudio (clears audio_live) always wins.
        assert!(!live_audio_enabled(false, false, true));
        assert!(!live_audio_enabled(false, true, true));
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

    /// An unsupported serving speed is refused where it is typed, naming
    /// the values that work, rather than surfacing later from config parsing.
    #[cfg(feature = "floppybridge")]
    #[test]
    fn floppy_bridge_speed_flag_refuses_an_unsupported_percentage() {
        let err = parse(&["--floppy-bridge-speed", "df0", "120"])
            .expect_err("120 is not a supported serving speed");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("100, 125, 150, 175, or 200"),
            "unexpected error: {msg}"
        );
    }

    /// A real drive can be asked for entirely from the command line, with no
    /// config file: the flags have to create the bay's table, not just fill
    /// one in.
    // The flags only exist in a build that can attach a physical drive.
    #[cfg(feature = "floppybridge")]
    #[test]
    fn floppy_bridge_flags_configure_a_bay_with_no_config_file() -> Result<()> {
        let args = parse(&[
            "--floppy-bridge",
            "df1",
            "greaseweazle",
            "--floppy-bridge-port",
            "df1",
            "/dev/ttyACM0",
            "--floppy-bridge-cable",
            "df1",
            "b",
            "--floppy-bridge-writable",
            "df1",
            "--floppy-bridge-mode",
            "df1",
            "compatible",
            "--floppy-bridge-density",
            "df1",
            "dd",
            "--floppy-bridge-speed",
            "df1",
            "125",
            "--floppy-bridge-auto-cache",
            "df1",
        ])?;

        let raw = Config::load_raw(None, &args.overrides)?;
        let cfg = Config::try_from(raw)?;
        let bridge = cfg.floppy.bridges[1].as_ref().expect("df1 bridged");
        assert_eq!(
            bridge.driver,
            copperline::config::BridgeDriver::Greaseweazle
        );
        assert_eq!(bridge.port.as_deref(), Some("/dev/ttyACM0"));
        assert_eq!(bridge.cable, copperline::config::BridgeCable::DriveB);
        assert!(!bridge.write_protected);
        assert_eq!(bridge.mode, copperline::config::BridgeSpeedMode::Compatible);
        assert_eq!(bridge.density, copperline::config::BridgeDensity::Dd);
        assert_eq!(bridge.speed, 125);
        assert!(bridge.auto_cache);
        // Untouched bays stay as they were.
        assert!(cfg.floppy.bridges[0].is_none());
        Ok(())
    }

    /// The flag means "this bay is a real drive", so it displaces an image the
    /// config file put there rather than colliding with it -- a bay cannot
    /// hold both, and the command line wins.
    // The flags only exist in a build that can attach a physical drive.
    #[cfg(feature = "floppybridge")]
    #[test]
    fn floppy_bridge_flag_replaces_a_configured_image() -> Result<()> {
        let path = std::env::temp_dir().join(format!(
            "copperline-bridge-cli-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "[floppy.df0]\npath = \"workbench.adf\"\nwrite_protected = true\n",
        )?;

        let args = parse(&["--floppy-bridge", "df0", "greaseweazle"])?;
        let raw = Config::load_raw(Some(&path), &args.overrides)?;
        let cfg = Config::try_from(raw)?;
        let _ = std::fs::remove_file(&path);

        assert!(cfg.floppy.bridges[0].is_some(), "the bay is a real drive");
        assert!(cfg.floppy.drives[0].is_none(), "and has no image left");
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
    fn floppy_speed_override_parses_with_alias() -> Result<()> {
        assert_eq!(
            parse(&["--floppy-speed", "800"])?.overrides.floppy_speed,
            Some(800)
        );
        // 0 selects turbo.
        assert_eq!(
            parse(&["--fdd-speed", "0"])?.overrides.floppy_speed,
            Some(0)
        );
        let err = parse(&["--floppy-speed", "150"]).unwrap_err();
        assert!(err.to_string().contains("100, 200, 400, 800"), "{err:#}");
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
            "--jit",
        ])?;
        assert_eq!(args.overrides.model.as_deref(), Some("A1200"));
        assert_eq!(args.overrides.cpu.as_deref(), Some("68030"));
        assert_eq!(args.overrides.cpu_clock_mhz, Some(50.0));
        assert_eq!(args.overrides.fpu, Some(true));
        assert_eq!(args.overrides.cpu_jit, Some(true));
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
