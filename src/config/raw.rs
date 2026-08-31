//! Raw TOML view of the configuration: `RawConfig`, its section
//! structs, and the file-parsing entry point.
use super::*;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
/// Read and parse a config file into its raw TOML view.
pub(crate) fn raw_from_path(path: &Path) -> Result<RawConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&text).map_err(|e| {
        let mut err = anyhow::Error::new(e);
        // A backslash in a double-quoted TOML string is an escape character,
        // so a Windows path like "C:\Kickstarts\KICK31.ROM" fails to parse on
        // "\K". The bare "invalid escape sequence" message rarely makes that
        // connection, so point at the fix.
        if err.to_string().contains("escape") {
            err = err.context(
                "a backslash in a double-quoted string is an escape character; \
                 for Windows paths use single quotes ('C:\\dir\\file'), double \
                 the backslashes, or use forward slashes",
            );
        }
        err.context(format!("parsing config {}", path.display()))
    })
}

// --- raw deserialization (one nested struct per [section]) ---------------

// `Serialize` lets the launcher write a configured machine back out as TOML
// (the configuration screen's Save). The `skip_serializing_if` attributes keep
// the output minimal -- only fields and sections the user actually set are
// emitted, matching the style of the hand-written `*.example.toml`. The
// `toml` serializer requires every top-level scalar key to be emitted before
// any `[table]`, so the top-level scalars (`rom`, `extended_rom`, `fmv_rom`,
// `identify`) are declared first, ahead of the section tables and the `zorro`
// array of tables. Field declaration order otherwise mirrors deserialization,
// which is order-independent.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rom: Option<String>,
    /// Extended ROM image (CD32 512K at $E00000, CDTV 256K at $F00000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) extended_rom: Option<String>,
    /// CD32 Full Motion Video cartridge ROM (bundled open 256 KiB default;
    /// an empty string leaves the module unfitted).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fmv_rom: Option<String>,
    /// `identify = false` drops the Copperline identification board from the
    /// autoconfig chain (default: present).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) identify: Option<bool>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) cd: RawCd,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) debug: RawDebug,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) cpu: RawCpu,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) emulation: RawEmulation,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) memory: RawMemory,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) machine: RawMachine,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) chipset: RawChipset,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) audio: RawAudio,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) ide: RawIde,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) scsi: RawScsi,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) lide: RawLide,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) a2065: RawA2065,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) toccata: RawToccata,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) mhi: RawMhi,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) hostsocket: RawHostSocket,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) zz9k: RawZz9k,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) rtg: RawRtg,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) floppy: RawFloppy,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) display: RawDisplay,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) input: RawInput,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) serial: RawSerial,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) parallel: RawParallel,
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) whdload: RawWhdload,
    /// `[[filesys]]` host-directory mount entries, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) filesys: Vec<RawFilesysMount>,
    /// `[[host_disk]]` real host disks, in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) host_disk: Vec<RawHostDisk>,
    /// `[[zorro]]` board entries, configured in file order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) zorro: Vec<RawZorroBoard>,
    /// `[paths]`: where this machine's output goes and where its file
    /// dialogs open. Absent until somebody moves something, and every
    /// entry within it optional, so a configuration that never mentions
    /// directories behaves exactly as one written before the section
    /// existed.
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) paths: crate::pathconf::Paths,
}

impl RawConfig {
    /// Serialize this raw config back to TOML text for the configuration
    /// screen's Save. Only non-default fields are written (see the
    /// `skip_serializing_if` attributes), so the result reads like the
    /// hand-written example configs.
    #[cfg_attr(not(feature = "frontend"), allow(dead_code))]
    pub(crate) fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).context("serializing configuration to TOML")
    }

    /// The `[paths]` section, for the startup that must put it in force
    /// before building a [`Config`] -- the conversion resolves the implicit
    /// battery-RAM files through the paths already in force.
    pub fn paths(&self) -> crate::pathconf::Paths {
        self.paths.clone()
    }

    /// The configured menu size, for the paths that put a window up before a
    /// whole [`Config`] has been built.
    pub fn menu_scale(&self) -> MenuScale {
        self.display
            .menu_scale
            .as_deref()
            .and_then(|s| parse_menu_scale(s).ok())
            .unwrap_or_default()
    }

    /// The configured live-audio state (`[audio] output_enabled`), defaulting to
    /// on when unset -- matching [`AudioConfig`]'s default. Lets the binary seed
    /// the config-screen session audio without reaching into private raw fields.
    pub fn audio_output_enabled(&self) -> bool {
        self.audio.output_enabled.unwrap_or(true)
    }

    // --- programmatic construction for alternative frontends ------------
    //
    // The raw sections stay crate-private; the small surface a
    // publisher-kit player (crates/copperline-player) needs to describe
    // its baked machine and sidecar payload is named here instead, and
    // everything still funnels through the one TryFrom validation.

    /// Parse configuration TOML text into its raw form, exactly as
    /// [`Config::load_raw`] would read it from a file.
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("parsing configuration TOML")
    }

    /// `[machine] profile`, by name ("A500", "CD32", ...): the model whose
    /// defaults the conversion to [`Config`] starts from.
    pub fn set_machine_profile(&mut self, model: &str) {
        self.machine.profile = Some(model.to_string());
    }

    /// `[memory]` size overrides on the profile ("2M", "8M", ...); `None`
    /// keeps the profile's own figure.
    pub fn set_memory_overrides(
        &mut self,
        chip: Option<&str>,
        fast: Option<&str>,
        slow: Option<&str>,
    ) {
        if let Some(size) = chip {
            self.memory.chip = Some(size.to_string());
        }
        if let Some(size) = fast {
            self.memory.fast = Some(size.to_string());
        }
        if let Some(size) = slow {
            self.memory.slow = Some(size.to_string());
        }
    }

    /// `[cd] image`: the disc in the machine's CD drive at power-on.
    pub fn set_cd_image(&mut self, path: &Path) {
        self.cd.image = Some(path.to_string_lossy().into_owned());
    }

    /// `[floppy.df0] path`: the disk in the internal drive at power-on.
    pub fn set_boot_floppy(&mut self, path: &Path) {
        self.floppy.df0 = Some(RawFloppyDrive {
            path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        });
    }

    /// The `[display]` defaults a game manifest carries: shader and bezel
    /// by their config names, and whether the session opens fullscreen.
    /// Only what is given is set, so a later overlay still wins.
    pub fn set_display_defaults(
        &mut self,
        shader: Option<&str>,
        bezel: Option<&str>,
        fullscreen: Option<bool>,
    ) {
        if let Some(name) = shader {
            self.display.shader = Some(name.to_string());
        }
        if let Some(name) = bezel {
            self.display.bezel = Some(RawBezel::Named(name.to_string()));
        }
        if let Some(on) = fullscreen {
            self.display.full_screen = Some(on);
        }
    }

    /// Whether the session opens fullscreen, for a frontend's own
    /// command-line override.
    pub fn set_fullscreen(&mut self, on: bool) {
        self.display.full_screen = Some(on);
    }

    /// `[display] status_bar`, for the player's permanently bar-less
    /// presentation.
    pub fn set_status_bar(&mut self, on: bool) {
        self.display.status_bar = Some(on);
    }

    /// Layer a player settings file ([`PLAYER_SETTINGS_FILE`], written by
    /// the menu's write-through) over this configuration: every field the
    /// player persists that the overlay actually carries replaces the base
    /// value, and anything else keeps the manifest's default -- so a
    /// partial or hand-edited file still behaves.
    pub fn merge_player_settings(&mut self, overlay: &RawConfig) {
        fn take<T: Clone>(base: &mut Option<T>, over: &Option<T>) {
            if over.is_some() {
                *base = over.clone();
            }
        }
        take(
            &mut self.display.pixel_aspect,
            &overlay.display.pixel_aspect,
        );
        take(&mut self.display.scaling, &overlay.display.scaling);
        take(&mut self.display.autocrop, &overlay.display.autocrop);
        take(&mut self.display.menu_scale, &overlay.display.menu_scale);
        take(&mut self.display.shader, &overlay.display.shader);
        take(
            &mut self.display.shader_strength,
            &overlay.display.shader_strength,
        );
        take(&mut self.display.tint, &overlay.display.tint);
        take(&mut self.display.bezel, &overlay.display.bezel);
        take(&mut self.display.tv_h_centre, &overlay.display.tv_h_centre);
        take(&mut self.display.tv_v_centre, &overlay.display.tv_v_centre);
        take(&mut self.display.full_screen, &overlay.display.full_screen);
        take(&mut self.input.port1, &overlay.input.port1);
        take(&mut self.input.port2, &overlay.input.port2);
        take(&mut self.input.joystick, &overlay.input.joystick);
        take(&mut self.input.autofire_hz, &overlay.input.autofire_hz);
        take(&mut self.audio.output_device, &overlay.audio.output_device);
        take(
            &mut self.audio.output_enabled,
            &overlay.audio.output_enabled,
        );
        take(&mut self.audio.audio_filter, &overlay.audio.audio_filter);
    }
}

/// `[display] bezel` as it may be written. It named a single frame to turn
/// on before there was a choice of them, so the boolean it took then is
/// still accepted: `true` means whichever style that was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawBezel {
    On(bool),
    Named(String),
}

impl RawBezel {
    pub(super) fn resolve(&self) -> Result<BezelStyle> {
        match self {
            RawBezel::On(true) => Ok(BezelStyle::Model1084),
            RawBezel::On(false) => Ok(BezelStyle::None),
            RawBezel::Named(s) => parse_bezel(s),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDisplay {
    /// "tv" (default, mask deep overscan like a CRT bezel) or "full".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) overscan: Option<String>,
    /// Horizontal centring of the TV presentation in lo-res pixels,
    /// positive right (default 0): a monitor's H-CENTER control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tv_h_centre: Option<i32>,
    /// Vertical centring of the TV presentation in scan lines, positive
    /// down (default 0): a monitor's V-CENTER control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tv_v_centre: Option<i32>,
    /// "tv" (default, 4:3 CRT pixel aspect) or "square" (1:1 host
    /// pixels; a lo-res display is an exact 2x2 of its bitmap).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pixel_aspect: Option<String>,
    /// "smooth" (default, aspect-fit with filtering) or "integer" (whole
    /// -number multiples of the canvas, centred, point-sampled).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scaling: Option<String>,
    /// Crop the window presentation to the display window the hardware
    /// programs (default false). Window-only; captures keep their
    /// configured aperture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) autocrop: Option<bool>,
    /// Motion-adaptive deinterlacing of interlaced content (default
    /// true); false line-doubles every field as it arrives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) deinterlace: Option<bool>,
    /// CRT phosphor persistence fraction, 0.0 (off, default) to 0.95.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) phosphor: Option<f32>,
    /// Window shader pass: "none" (default), "scanlines", "mask", "crt",
    /// or the path of a `.wgsl` file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shader: Option<String>,
    /// Shader mix, 0.0 (invisible) to 1.0 (full effect, the default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) shader_strength: Option<f32>,
    /// Monitor front around the window picture: "off" (default), "1084" or
    /// "classic". `true`/`false` are still taken, from when there was only
    /// the one frame to turn on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bezel: Option<RawBezel>,
    /// Folder of PNG stickers drawn onto the bezel; unset or empty draws
    /// none. An optional `stickers.toml` inside places them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bezel_stickers: Option<String>,
    /// Performance overlay in the top-right of the display (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) perf_overlay: Option<bool>,
    /// Screen tint: "none" (default), "bw", "green", "amber", or "sepia".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tint: Option<String>,
    /// Size of the pop-up menu: "1x" (default) or "2x".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) menu_scale: Option<String>,
    /// Open fullscreen at start (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) full_screen: Option<bool>,
    /// Show the status bar at start (default true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) status_bar: Option<bool>,
}

/// A host disk named on the command line, before it is checked against the
/// machine. The attachment point is the token form (`ide-slave`, `scsi3`);
/// unset means the default.
#[derive(Debug, Clone, PartialEq)]
pub struct HostDiskArg {
    pub device: String,
    pub attach: Option<String>,
    pub read_only: bool,
}

/// `[whdload]` direct WHDLoad boot (src/whdload.rs): stage a WHDLoad game
/// package and boot straight into it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWhdload {
    /// Game to boot: an `.lha` archive or a directory holding a `.slave`.
    /// `--whdload` overrides it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) game: Option<String>,
    /// Where extracted packages, their saves and the staged boot volumes
    /// live; defaults to `whdload/` in the per-user configuration
    /// directory. The launcher calls it the save directory, which is the
    /// part of it a person cares about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library: Option<String>,
    /// A directory of WHDLoad packages, searched for games to list. Unlike
    /// `game` -- one package -- this is a collection, and the launcher's
    /// Library page lists what is in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) games: Option<String>,
    /// Directory scanned for Kickstart images to stage into
    /// `Devs:Kickstarts/` (and to boot the machine from). When unset, the
    /// directory of an explicit `rom` and `<library>/Kickstarts` are tried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kickstarts: Option<String>,
    /// Extra options appended to the generated WHDLoad command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) args: Option<String>,
    /// The WHDLoad distribution archive (`WHDLoad_usr.lha`). When unset the
    /// copy bundled with the release is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) whd_package: Option<String>,
    /// The Soft-Kicker archive (`skick*.lha`), whose `.RTB` relocation
    /// tables accompany raw Kickstart images. When unset the bundled copy
    /// is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) skick_package: Option<String>,
    /// Which machine a package boots on: `auto` derives one from the slave
    /// header, `copperline` uses the machine this configuration describes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) machine_type: Option<WhdloadMachine>,
    /// Where the scanned library is written: one entry a game, with the
    /// metadata a scan resolved for it. Defaults to `whdload/library/db.json`
    /// in the per-user configuration directory.
    ///
    /// Configuration-file only, deliberately: it and `library_cache` say
    /// where the Library page keeps its own working files, which is not
    /// part of describing a machine and so has no row in the launcher.
    /// Parsed and ignored in a build without the `game-library` feature, so
    /// a configuration written by a full build still loads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library_db: Option<String>,
    /// Where a scan keeps what it downloaded -- cover art, and the snapshot
    /// of the online database it matched against. Defaults to
    /// `whdload/library/cache`. Throwing it away costs a re-download and
    /// nothing else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library_cache: Option<String>,
    /// Whether the launcher offers WHDLoad at all: its entry in the
    /// left-hand navigation, the Library and Configuration pages behind
    /// it, and the work those do. Defaults to on.
    ///
    /// Off, the launcher does none of it -- no entry, no pages, no game
    /// database read, no cover worker, no scan. It does not stop a game
    /// booting: `--whdload` and `[whdload] game` are explicit instructions
    /// and still do what they say, so scripts and headless runs are
    /// unaffected.
    ///
    /// Library-only, and ignored as the `library_*` keys are without the
    /// feature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
}

/// Which machine a WHDLoad package boots on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhdloadMachine {
    /// Derive it from the slave header: the canonical WHDLoad host, an
    /// A1200 with 8 MiB of fast RAM, which satisfies every slave flag.
    #[default]
    Auto,
    /// Boot the package on the machine this configuration describes,
    /// whatever that is. An explicit `[machine]`, `rom` or `[memory]` wins
    /// under `auto` too; this makes the whole machine the choice rather
    /// than the parts that were named.
    Copperline,
}

/// One `[[filesys]]` entry (experimental): a host directory exported to the
/// guest as the AmigaDOS device `HOSTFS<n>:` (n = position in the config).
/// `[[host_disk]]`: a real disk of the host's, given to the machine.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHostDisk {
    /// The host's identifier for the disk, as `--list-disks` prints it.
    pub(crate) device: String,
    /// Opaque identity written by the launcher. Optional for hand-written and
    /// older configurations, which are resolved by exact device name only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fingerprint: Option<String>,
    /// Set only by a command-line override or live launcher selection. Never
    /// serialized: reading a file is not fresh confirmation of attached media.
    #[serde(skip)]
    pub(crate) identity_confirmed: bool,
    /// Where the machine sees it: `ide-master` (the default), `ide-slave`,
    /// `lide0-master`, `lide0-slave`, `lide1-master`, `lide1-slave`, or
    /// `scsi0`..`scsi6`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attach: Option<String>,
    /// Protect the disk from the guest. Absent means read-only: real media is
    /// writable only when the user has said so explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) read_only: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFilesysMount {
    /// Host directory to export.
    pub(crate) path: String,
    /// AmigaDOS volume name; defaults to the directory's name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) volume: Option<String>,
    /// Boot priority (-128..=127); defaults to -128, which mounts the
    /// volume but never offers it as a boot candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bootpri: Option<i8>,
    /// Export the directory write-protected: the guest sees the volume as a
    /// read-only disk and every write fails. Defaults to false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) readonly: Option<bool>,
}

/// `[input]` host-input preferences: which controller device is plugged into
/// each game port, and the host source for the joystick port. The status-bar
/// toggle and `Cmd+J` / `Alt+J` flip the joystick source live.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawInput {
    /// Initial joystick input source: "gamepad" (default) or "keyboard".
    /// ("auto" is still accepted for backward compatibility and maps to
    /// "gamepad".)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) joystick: Option<String>,
    /// Device in game port 1: "mouse" (default), "joystick", "cd32",
    /// "analogue", or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port1: Option<String>,
    /// Device in game port 2: same values; defaults to "joystick"
    /// ("cd32" on the CD32 profile).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) port2: Option<String>,
    /// Host mouse sensitivity, 0-100 (default 50).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_sensitivity: Option<u16>,
    /// When the host mouse is grabbed: "click" (default), "auto", or
    /// "manual".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mouse_capture: Option<String>,
    /// Autofire rate in Hz for the fire button, or 0 (the default) for off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) autofire_hz: Option<u8>,
}

/// `[serial]` host wiring for Paula's serial (a.k.a. MIDI) port.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawSerial {
    /// "stdout" (default), "off", "midi", "tcp", "tcp-connect", or "pty".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<String>,
    /// Host MIDI output endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_out: Option<String>,
    /// Host MIDI input endpoint name (substring match); MIDI mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) midi_in: Option<String>,
    /// MT-32 control ROM image; needed when midi_out = "mt32".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_control_rom: Option<String>,
    /// MT-32 PCM ROM image; needed when midi_out = "mt32".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_pcm_rom: Option<String>,
    /// Show the MT-32's front panel under the status bar (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_panel: Option<bool>,
    /// Its display: "mt32" (default), "superjv", "sseries", or "oled".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mt32_lcd: Option<String>,
    /// Coppersynth's soundfont (.sf2, or a .zip holding one); unset means
    /// the bundled default's search path (COPPERLINE_SOUNDFONT, beside
    /// the executable, share/).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) coppersynth_soundfont: Option<String>,
    /// Coppersynth's MT-32 mode: "auto" (default; translates once MT-32
    /// sysex is seen), "on", or "off".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) coppersynth_mt32_mode: Option<String>,
    /// Show Coppersynth's front panel under the status bar (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) coppersynth_panel: Option<bool>,
    /// TCP listen address; tcp mode only. Defaults to 127.0.0.1:1234.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) listen: Option<String>,
    /// Remote host:port to dial; tcp-connect mode only, and required there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) connect: Option<String>,
    /// AT*T1/AT*T0 default at power-on: telnet NVT translation (the
    /// WiModem extra) on by default. Modem mode only. Defaults to false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) telnet: Option<bool>,
    /// `[serial.phonebook]`: dialable numbers a guest's ATD can look up,
    /// number -> "host:port" (or a bare host, which dials the modem's
    /// default port). Modem mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) phonebook: Option<std::collections::BTreeMap<String, String>>,
    /// A scripted session file to replay instead of dialing out over TCP
    /// (`docs/guide/modem.md`'s "Scripted sessions" section has the file
    /// format). Modem mode only; incompatible with `listen` (the scripted
    /// transport has no inbound side).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
}

/// `[parallel]` peripheral selection for the Amiga Centronics parallel port.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawParallel {
    /// Which device is plugged in: `none`, `printer`, or `sampler`. When
    /// omitted, a bare `output` path implies `printer` (back-compat) and
    /// otherwise the port is empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device: Option<String>,
    /// Printer raw byte-stream output path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output: Option<String>,
    /// Sampler host capture device name (substring match); absent = default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sampler_input: Option<String>,
    /// Sampler input gain in decibels (preamp); absent = 0 dB (unity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sampler_gain: Option<f32>,
}

/// A drive image entry in `[ide]`/`[scsi]`. Accepts either a bare path string
/// (`master = "disk.hdf"`) or a table carrying an explicit volume-name override
/// and/or boot priority (`master = { path = "games/", name = "Games",
/// bootpri = 5 }`). It serializes back to the bare string when neither
/// override is set, so existing minimal configs round-trip unchanged.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawDrive {
    pub(crate) path: String,
    pub(crate) name: Option<String>,
    /// Boot priority written into the synthesized RDB's `de_BootPri`
    /// (-128..=127); defaults to 0, the priority HDToolBox gives a hard-disk
    /// boot partition. -128 mounts the partition without offering it for boot.
    pub(crate) bootpri: Option<i8>,
    /// "ofs" or "ffs"; only valid on a host-directory mount. Defaults to FFS
    /// when absent.
    pub(crate) filesystem: Option<String>,
}

impl RawDrive {
    pub(crate) fn from_path(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            name: None,
            bootpri: None,
            filesystem: None,
        }
    }
}

impl Serialize for RawDrive {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.name.is_none() && self.bootpri.is_none() && self.filesystem.is_none() {
            // No overrides: a plain string keeps saved configs minimal.
            return serializer.serialize_str(&self.path);
        }
        use serde::ser::SerializeMap;
        let len = 1
            + usize::from(self.name.is_some())
            + usize::from(self.bootpri.is_some())
            + usize::from(self.filesystem.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("path", &self.path)?;
        if let Some(name) = &self.name {
            map.serialize_entry("name", name)?;
        }
        if let Some(bootpri) = &self.bootpri {
            map.serialize_entry("bootpri", bootpri)?;
        }
        if let Some(filesystem) = &self.filesystem {
            map.serialize_entry("filesystem", filesystem)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for RawDrive {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DriveVisitor;
        impl<'de> serde::de::Visitor<'de> for DriveVisitor {
            type Value = RawDrive;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "a drive image path, or a table with `path` and optional `name`/`bootpri`",
                )
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<RawDrive, E> {
                Ok(RawDrive::from_path(v))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<RawDrive, E> {
                Ok(RawDrive::from_path(v))
            }
            fn visit_map<A>(self, mut map: A) -> std::result::Result<RawDrive, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut path: Option<String> = None;
                let mut name: Option<String> = None;
                let mut bootpri: Option<i8> = None;
                let mut filesystem: Option<String> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "path" => {
                            if path.is_some() {
                                return Err(serde::de::Error::duplicate_field("path"));
                            }
                            path = Some(map.next_value()?);
                        }
                        "name" => {
                            if name.is_some() {
                                return Err(serde::de::Error::duplicate_field("name"));
                            }
                            name = Some(map.next_value()?);
                        }
                        "bootpri" => {
                            if bootpri.is_some() {
                                return Err(serde::de::Error::duplicate_field("bootpri"));
                            }
                            bootpri = Some(map.next_value()?);
                        }
                        "filesystem" => {
                            if filesystem.is_some() {
                                return Err(serde::de::Error::duplicate_field("filesystem"));
                            }
                            filesystem = Some(map.next_value()?);
                        }
                        other => {
                            return Err(serde::de::Error::unknown_field(
                                other,
                                &["path", "name", "bootpri", "filesystem"],
                            ));
                        }
                    }
                }
                let path = path.ok_or_else(|| serde::de::Error::missing_field("path"))?;
                Ok(RawDrive {
                    path,
                    name,
                    bootpri,
                    filesystem,
                })
            }
        }
        deserializer.deserialize_any(DriveVisitor)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawIde {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) master: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slave: Option<RawDrive>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawScsi {
    /// Host adapter to fit: "a2091" (Zorro II, default), "a4091" (Zorro
    /// III), or "a3000" (the motherboard SDMAC, default on an A3000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) controller: Option<String>,
    /// Boot ROM image. For split even/odd A2091 EPROM dumps, `rom` is the
    /// even half and `rom_odd` the odd half.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom_odd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit0: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit1: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit2: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit3: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit4: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit5: Option<RawDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit6: Option<RawDrive>,
}

/// `[lide]` `lide.device`-compatible Zorro II IDE board.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLide {
    /// Board personality: "ripple" (default once the section is present),
    /// "ride", or "atbus2008".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) board: Option<String>,
    /// Boot ROM image (a `lide.rom`/`lide-atbus.rom` release download, 32768
    /// bytes). Absent on a fitted board defaults to the bundled `lide.rom`;
    /// an explicit empty string opts out and keeps the board in
    /// hardware-only mode: no autoboot, drives still work under a
    /// disk-loaded `lide.device`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom: Option<String>,
    /// Optional second flash bank (e.g. `cdfs.rom`), also 32768 bytes.
    /// Requires `rom`; not valid on the AT-Bus 2008 personality, which has
    /// no banking. Absent alongside a fitted `rom` defaults to the bundled
    /// `cdfs.rom` (RIPPLE/RIDE only); an explicit empty string opts out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom_bank2: Option<String>,
    /// Drive images, in (channel, master/slave) order: index 0-1 are
    /// channel 0's master/slave, index 2-3 are channel 1's (RIPPLE only).
    ///
    /// Deprecated in favour of the `drive0`..`drive3` keys below, which can
    /// leave a slot empty -- a positional array cannot express "channel 0
    /// slave only", so this form forced the launcher to hide any slot past
    /// the first empty one. Still read, so configs written before the named
    /// keys existed keep working; never written back out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) drives: Vec<RawDrive>,
    /// Channel 0 master. Named per slot, like `[ide]`'s `master`/`slave`
    /// and `[scsi]`'s `unit0`..`unit6`, so any slot can be filled or left
    /// empty independently of the others.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drive0: Option<RawDrive>,
    /// Channel 0 slave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drive1: Option<RawDrive>,
    /// Channel 1 master (RIPPLE only -- RIDE and AT-Bus 2008 have one
    /// channel).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drive2: Option<RawDrive>,
    /// Channel 1 slave (RIPPLE only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drive3: Option<RawDrive>,
}

impl RawLide {
    /// The four drive slots as named keys, with the deprecated positional
    /// `drives` array filled in underneath: a named key always wins, so a
    /// config carrying both is unambiguous rather than order-dependent.
    pub(crate) fn drive_slots(&self) -> [Option<RawDrive>; 4] {
        let named = self.named_drive_slots();
        std::array::from_fn(|i| named[i].clone().or_else(|| self.drives.get(i).cloned()))
    }

    /// Just the named `driveN` keys, without the deprecated array
    /// underneath. Validation checks these per slot so it can name the key
    /// at fault; a slot filled from the legacy array is covered by that
    /// array's own length rule instead, since reporting it as `driveN`
    /// would name a key the config never wrote.
    pub(crate) fn named_drive_slots(&self) -> [Option<RawDrive>; 4] {
        [
            self.drive0.clone(),
            self.drive1.clone(),
            self.drive2.clone(),
            self.drive3.clone(),
        ]
    }
}

/// `[a2065]` Ethernet board. Fitting the board enables host networking, which
/// is non-deterministic.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawA2065 {
    /// Host network backend: "loopback", "nat", "bridge", or "none" for an
    /// isolated NIC. Absent means no A2065 board is fitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) net: Option<String>,
    /// Host adapter identifier used by `net = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interface: Option<String>,
}

/// `[toccata]` MacroSystem Toccata sound board.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawToccata {
    /// Fit the board. Absent/false means no Toccata is on the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
}

/// `[mhi]` MHI virtual MPEG audio decoder board.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMhi {
    /// Fit the board. Absent/false means no MHI board is on the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
}

/// `[hostsocket]` bundled bsdsocket.library board: a host-side TCP/IP stack
/// presented to the guest as `bsdsocket.library`, with no guest network
/// stack to boot (see `crate::hostsocket`). Fitting the board with a real
/// backend enables host networking, which is non-deterministic; the
/// "loopback" backend stays deterministic.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawHostSocket {
    /// Host network backend: "loopback", "nat", "bridge", "host" (the
    /// Amiberry-style host-socket backend -- see
    /// `crate::hostsocket::board_config`'s own `transport` parameter, and
    /// this crate's config-resolution code for what it implies:
    /// `interface`/`address`/`gateway` are rejected, and `resolver`
    /// defaults to `"host"`), or "none" for a board with a dead wire.
    /// Absent means no HostSocket board is fitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) net: Option<String>,
    /// Host adapter identifier used by `net = "bridge"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) interface: Option<String>,
    /// DNS server queried when `resolver = "dns"`. Defaults to Copperline
    /// NAT's forwarder (10.0.2.3); ignored by the default host resolver.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dns_server: Option<String>,
    /// `gethostname()`'s return value. Purely cosmetic; defaults to "amiga".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hostname: Option<String>,
    /// Interface address ("a.b.c.d" or "a.b.c.d/prefix"), meaningful only
    /// under `net = "bridge"`: the default (Copperline NAT's own virtual
    /// address) is meaningless on a real LAN, where this must match one.
    /// Rejected under `net = "host"` (which has no interface address of
    /// its own at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) address: Option<String>,
    /// Default gateway, meaningful only under `net = "bridge"`; same
    /// reasoning as `address` above, including rejection under
    /// `net = "host"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gateway: Option<String>,
    /// `gethostbyname()`'s resolver strategy: "host" asks Copperline's own
    /// process to resolve via the host OS resolver on a background
    /// thread, ignoring `dns_server` entirely; "dns" queries `dns_server`
    /// directly over the board's own network backend instead. Defaults to
    /// "host" under `net = "nat"`/`"bridge"`/`"host"` when left unset --
    /// the thing that works without a `dns_server` hand-matched to the
    /// backend (and `net = "host"`'s own underlying smoltcp interface is
    /// hardcoded to "loopback", which couldn't reach a real one anyway);
    /// set "dns" explicitly (with `dns_server`) to opt back into a
    /// specific resolver. Explicit "host" is rejected under
    /// `"loopback"`/`"none"`, where it would silently break the backend's
    /// own determinism guarantee (there is no sane default there either,
    /// so those backends simply get no resolver key at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolver: Option<String>,
}

/// `[zz9k]` bundled ZZ9000 SDK crypto board: a register-compatible subset
/// of the MNT ZZ9000's SDK v2 service platform (CORE + MEMORY + CRYPTO)
/// with the crypto computed host-side (see `crate::zz9k` and
/// docs/internals/zz9k.md). Pure compute -- fitting it keeps the machine
/// deterministic.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawZz9k {
    /// Fit the board. Absent/false means no board is on the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    /// Bus generation: 3 (Zorro III, needs a 32-bit CPU) or 2 (Zorro II,
    /// window fixed at 4M -- the only Zorro II size the SDK transport
    /// accepts shared-buffer allocations for). Defaults to 3 on a 32-bit
    /// CPU, 2 otherwise, matching the SDK's own probe order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) zorro: Option<u8>,
    /// Zorro III window size (power of two, 1M..256M; default "4M").
    /// Zorro II ignores this and always maps 4M.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<String>,
    /// Answer the guest's ZZ9000.CFG `int2` key query with "use INT2
    /// (PORTS)" instead of the INT6 (EXTER) default for the completion
    /// interrupt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) int2: Option<bool>,
    /// Reserved deterministic DRBG seed (up to 64 hex digits). No current
    /// board operation draws randomness; the default (unset) is a fixed
    /// constant, so runs stay byte-reproducible either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) seed: Option<String>,
}

/// `[rtg]` graphics card: an RTG board on the Zorro chain.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRtg {
    /// Card to fit: "z3660", "picasso2", "picasso2plus", "graffityz2",
    /// "graffityz3", or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) card: Option<String>,
    /// Picasso II/II+ and Graffity display memory: "1M" or "2M" (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) vram: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCd {
    /// Path to a CUE/BIN, bare ISO, Nero NRG, or CHD CD image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) image: Option<String>,
    /// Insert the disc this many emulated seconds after power-on
    /// instead of at boot (CDTV; some discs only boot when inserted
    /// after the boot screen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) insert_delay: Option<f64>,
    /// CD32 NVRAM (save game EEPROM) backing file. Defaults to
    /// "cd32-nvram.bin" on CD32 machines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) nvram: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCpu {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fpu: Option<bool>,
    /// Override the CPU clock in MHz. Defaults to the model's stock speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) clock_mhz: Option<f64>,
    /// Model the on-chip instruction cache. Defaults on for the models
    /// that have one (all 020+).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) icache: Option<bool>,
    /// Model the on-chip data cache. Defaults on for the models that have
    /// one (030/040/060).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dcache: Option<bool>,
    /// 68060 only: what happens on the instructions the 68060 dropped from
    /// silicon (MOVEP, CHK2/CMP2, CAS2, misaligned CAS, 64-bit MUL/DIV, the
    /// unimplemented FPU subset). "trap" (default) is faithful - the OS
    /// needs 68060.library to emulate them; "native" executes them directly
    /// for systems without the library.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unimplemented: Option<String>,
    /// Fast CPU execution through the m68k core's batch/trace-JIT path.
    /// Not cycle-exact: the CPU behaves like an accelerator card. Defaults
    /// off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) jit: Option<bool>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEmulation {
    /// Deprecated and ignored: "real" was the only remaining timing model,
    /// so the option carried no information. Still accepted (and warned
    /// about) so existing configs that name it keep parsing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) power_on: Option<bool>,
    /// Skip the configuration screen when this file is opened in the
    /// launcher and run the machine at once (default false). Only the
    /// launcher reads it; a machine started from the command line is
    /// already past the screen this key skips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) auto_launch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pacing_budget: Option<String>,
    /// Best-effort realtime-like thread priority for the pacer and audio
    /// threads (default false). See `src/priority.rs`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) realtime_priority: Option<bool>,
    /// UI warp/turbo speed: "2x", "4x", "8x", "16x", or "max" (default).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warp_speed: Option<String>,
    /// Warp boot: run unpaced from power-on until the boot storage has
    /// been idle for `warp_boot_idle` emulated seconds (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warp_boot: Option<bool>,
    /// The storage-idle threshold for `warp_boot`, in emulated seconds
    /// (default 10). Must outlast the boot's longest storage-quiet
    /// stretch (a big-RAM machine's SetPatch MMU table build keeps the
    /// disk idle for seconds).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warp_boot_idle: Option<f64>,
    /// Warp boot until an absolute emulated timestamp, in seconds.
    /// Deterministic alternative to `warp_boot`; the two are mutually
    /// exclusive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warp_until: Option<f64>,
    /// Record rewind history from power-on (default false), so the rewind
    /// hotkey works outside the debugger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind: Option<bool>,
    /// Host memory (MiB) the rewind snapshot ring may hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind_budget_mb: Option<usize>,
    /// Emulated frames between rewind snapshots (one rewind step).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rewind_interval_frames: Option<u64>,
    /// Run-ahead input-latency reduction: present the frame `n` emulated
    /// frames in the future of the anchor each display refresh (0 = off).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) run_ahead_frames: Option<u8>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMemory {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fast: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slow: Option<String>,
    /// Cold-power-on RAM contents: "zero" (default), "random" (the fixed
    /// reproducible seed), "random:SEED", or a fixed 16-bit
    /// "pattern:WORD" / bare "0xWORD".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) init: Option<String>,
    /// Ramsey-controlled motherboard fast RAM size (e.g. "16M"); needs a
    /// Ramsey (A3000/A4000 profiles) and a 32-bit CPU. Sizes beyond 16M
    /// (up to 64M) fill the motherboard RAM expansion space and need the
    /// A4000's Ramsey-07.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) motherboard: Option<String>,
    /// CPU-slot (accelerator) fast RAM size at $08000000 (e.g. "64M", up
    /// to 128M); 32-bit CPUs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) accelerator: Option<String>,
    /// Zorro III autoconfig RAM size (e.g. "16M"); 32-bit CPUs only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) z3: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawZorroBoard {
    /// Path to a TOML board metadata file (see `src/zorro.rs` for the
    /// schema), resolved relative to the working directory.
    pub(crate) metadata: String,
    /// Per-board plugin setting overrides, layered over the manifest's
    /// `[config]` defaults (WASM plugin boards only). The launcher edits these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) config: Option<toml::Table>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawMachine {
    /// Machine profile name. Named `profile` (not `model`) so it never
    /// collides with `[cpu] model`: an uncommented profile line landing in
    /// the wrong table would otherwise be a confusing duplicate-key error.
    /// `model` stays accepted as a deprecated alias for old configs.
    #[serde(alias = "model", skip_serializing_if = "Option::is_none")]
    pub(crate) profile: Option<String>,
    /// Whether the $DC0000 RTC is fitted; defaults per profile (only the
    /// A500+ and CDTV ship with one, so the base A500/A600/A1200/etc. default
    /// to none). Set `rtc = true` to fit one, e.g. for an A600HD or a
    /// clock-equipped A1200.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc: Option<bool>,
    /// Which clock part fills the socket: `"MSM6242"` (OKI, most boards)
    /// or `"RP5C01"` (Ricoh, the A3000/A4000 part and the only protocol
    /// Linux/m68k drives on those models). Defaults per profile; setting
    /// it implies `rtc = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_chip: Option<String>,
    /// Power-on clock value: an integer (Unix seconds, UTC) or a string
    /// `"YYYY-MM-DD HH:MM[:SS]"` (the wall-clock time the guest reads).
    /// Seeds the battery clock and ticks it in emulated time so the
    /// guest-visible time is deterministic; implies `rtc = true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_time: Option<RawRtcTime>,
    /// Stop the seeded clock so every read returns `rtc_time` exactly.
    /// Only meaningful together with `rtc_time`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rtc_frozen: Option<bool>,
    /// Backing file for the RP5C01's battery RAM (the storage behind
    /// AmigaOS `battmem.resource` on the A3000/A4000), in the
    /// WinUAE/Amiberry `.nvram` file layout so files interchange between
    /// emulators. Defaults to `battmem.nvram` whenever an RP5C01 is
    /// fitted; an empty string keeps the battery registers session-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) battmem: Option<String>,
    /// Memory controller fitted, defaulting per profile: `none`, `ramsey-04`
    /// (A3000) or `ramsey-07` (A4000). Ramsey answers at $DE0000, which no
    /// other chip decodes, so it can also be fitted to a wedge machine to
    /// exercise the diagnostic tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mem_controller: Option<String>,
    /// Skip the ROM's scsi.device. Defaults to true only when the machine's
    /// built-in disk controller (Gayle or A4000 IDE, A3000 SDMAC SCSI) has no
    /// drives configured, where the driver would only cost boot time probing
    /// an empty bus; false everywhere else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rom_scsi_device_disable: Option<bool>,
}

/// `[machine] rtc_time` accepts both TOML notations for one instant: a
/// bare integer (Unix seconds) or a calendar string. Both funnel through
/// `crate::rtc::parse_rtc_time` at validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawRtcTime {
    Unix(i64),
    Text(String),
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDebug {
    /// Arm the custom-register access validator: report software using the
    /// chipset in ways the hardware ignores (absent registers, undefined
    /// bits, wrong-direction and byte accesses, DMA pointers past Agnus's
    /// reach), each with the PC or Copper address that did it. Off by
    /// default; it also arms the per-register last-writer table.
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) validate_chipset: bool,
    /// Report writes that land on memory the CPU has already executed.
    /// Off by default; costs a 1 MiB execution map while armed.
    #[serde(default, skip_serializing_if = "is_default")]
    pub(crate) detect_smc: bool,
    /// Log CPU accesses that no device decodes. Either `all`, or an address
    /// range like `"DD0000-DE0000"` (hex, end exclusive) to watch one window.
    /// Reads report the floating bus value they returned; writes report the
    /// value that went nowhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) log_unmapped: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawChipset {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video: Option<String>,
    /// Fine-grained chip overrides on top of the `revision` preset, for the
    /// mixed machines that really shipped (e.g. late A500: ECS Agnus with an
    /// OCS Denise). `agnus` accepts OCS / 8370 / 8371 / 8372 / 8372A / 8372B /
    /// 8374 / 8375 / ALICE; `denise` accepts OCS / 8362 / ECS / 8373 / LISA /
    /// 4203.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agnus: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) denise: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAudio {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) floppy_sounds: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) floppy_sounds_volume: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) channel_mode: Option<String>,
    // `filter` is accepted as an alias so a config that followed the #278
    // request (which spelled it `[audio] filter`) still loads under
    // deny_unknown_fields; `audio_filter` is canonical and matches
    // `--audio-filter`.
    #[serde(alias = "filter", skip_serializing_if = "Option::is_none")]
    pub(crate) audio_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stereo_separation: Option<u16>,
    /// Default `--audio-stems-mode` granularity list (`"master,source"`) for
    /// `--audio-stems` runs that don't pass `--audio-stems-mode` explicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stem_granularity: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFloppy {
    /// Number of wired floppy drives, DF0..DFN-1. Zero models machines with
    /// no built-in drive, such as the CDTV and CD32; the valid range is 0-4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drives: Option<u8>,
    /// Drive speed percentage (100/200/400/800) or 0 for turbo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df0: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df1: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df2: Option<RawFloppyDrive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) df3: Option<RawFloppyDrive>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFloppyDrive {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
    /// A playlist of images for this drive, cycled with the disk-swap
    /// key. When given, the first entry is the boot disk. May be used
    /// instead of `path`; if both appear, `path` is treated as the first
    /// entry followed by `paths`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) write_protected: Option<bool>,
    /// Attach a real drive to this bay instead of an image. The parser knows
    /// every FluxBridge driver name, then rejects any driver this build did
    /// not compile; the standard build currently enables `greaseweazle`
    /// (alias `gw`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge: Option<String>,
    /// Serial port the interface is on. Omitted, the driver finds its own
    /// device; name one when two interfaces are plugged in at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_port: Option<String>,
    /// How each track is captured: `normal` (the default; `fast` is accepted
    /// as upstream's own name for it), `compatible`, or `stalling`. `turbo` is
    /// refused by name -- it answers AmigaDOS calls rather than reading the
    /// disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_mode: Option<String>,
    /// Force a density instead of sensing it: `auto`, `dd`, or `hd`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_density: Option<String>,
    /// Which drive on the cable to select: `a`/`b` (IBM PC cabling) or
    /// `0`..`3` (Shugart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bridge_cable: Option<String>,
    /// Replay kept captures at real speed (`normal`/100) or double speed
    /// (`fast`/200, the default).
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "bridge_speed",
        rename = "replay_speed"
    )]
    pub(crate) bridge_speed: Option<RawReplaySpeed>,
}

/// The replay speed as a config file may spell it: a word, or one of the
/// percentages the setting used to take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawReplaySpeed {
    Percent(u16),
    Word(String),
}

/// Convert a parsed `[ide]`/`[scsi]`/`[lide]` drive entry into a `DriveImage`,
/// validating any volume-name override and the `filesystem` key. An
/// empty/whitespace name is treated as no override; AmigaDOS volume names
/// cannot contain ':' or '/' and the FFS/OFS root block stores at most 30
/// characters.
pub(super) fn drive_image(raw: RawDrive) -> Result<DriveImage> {
    let volume_name = match raw.name {
        None => None,
        Some(name) => {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                None
            } else if let Some(err) = crate::filesys::volume_name_error(trimmed) {
                bail!("drive name: {err}");
            } else {
                Some(trimmed.to_string())
            }
        }
    };
    let path = PathBuf::from(raw.path);
    let filesystem = match raw.filesystem {
        None => crate::diskimage::FileSystem::FFS,
        Some(fs) => {
            if !path.is_dir() {
                bail!(
                    "drive filesystem = {fs:?}: only applies to a host-directory mount, not \
                     an image file"
                );
            }
            match fs.trim().to_ascii_lowercase().as_str() {
                "ofs" => crate::diskimage::FileSystem::OFS,
                "ffs" => crate::diskimage::FileSystem::FFS,
                _ => bail!("drive filesystem = {fs:?} is not known (expected \"ofs\" or \"ffs\")"),
            }
        }
    };
    Ok(DriveImage {
        path,
        volume_name,
        boot_pri: raw.bootpri.unwrap_or(HARDFILE_DEFAULT_BOOT_PRI),
        filesystem,
    })
}
