// SPDX-License-Identifier: GPL-3.0-or-later

//! Bake the game manifest into the player binary.
//!
//! COPPERLINE_GAME_MANIFEST names the game.toml to build (see
//! game.example.toml for the format); unset, the example manifest is baked
//! so the crate still compiles for CI and exploration. The manifest is
//! read, validated, and emitted as a generated `baked` module of constants:
//! title, machine, payload kind and file name, display defaults, and the
//! icon bytes. The payload itself is never embedded -- the player resolves
//! it beside the binary at runtime.

use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    game: Game,
    payload: Payload,
    machine: Machine,
    #[serde(default)]
    display: Display,
    #[serde(default)]
    features: Features,
    #[serde(default)]
    branding: Branding,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Game {
    title: String,
    id: String,
    /// Optional release version, shown by --version and used by
    /// tools/publish to name the bundle. Defaults to "1.0".
    version: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    cd: Option<String>,
    adf: Option<String>,
    run: Option<RunPayload>,
    sha256: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunPayload {
    files: String,
    executable: String,
    #[serde(default)]
    args: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Machine {
    model: String,
    chip: Option<String>,
    fast: Option<String>,
    slow: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Display {
    shader: Option<String>,
    bezel: Option<String>,
    fullscreen: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Features {
    #[serde(default)]
    save_states: bool,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Branding {
    icon: Option<String>,
}

/// The models the root crate's `parse_machine_model` accepts, normalized
/// the same way, so a typo fails the build rather than the shipped binary.
const MODELS: [&str; 10] = [
    "A1000", "A500", "A500OCS", "A500PLUS", "A600", "A1200", "A3000", "A4000", "CDTV", "CD32",
];

fn normalize_model(s: &str) -> String {
    s.trim()
        .to_ascii_uppercase()
        .replace(['-', '_', ' '], "")
        .replace('+', "PLUS")
}

fn fail(msg: &str) -> ! {
    panic!("game manifest: {msg}");
}

fn opt_str(value: &Option<String>) -> String {
    match value {
        Some(s) => format!("Some({s:?})"),
        None => "None".to_string(),
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=COPPERLINE_GAME_MANIFEST");
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"));
    let manifest_path = match std::env::var_os("COPPERLINE_GAME_MANIFEST") {
        Some(path) => PathBuf::from(path),
        None => {
            println!(
                "cargo:warning=COPPERLINE_GAME_MANIFEST is not set; baking the example \
                 manifest (game.example.toml)"
            );
            crate_dir.join("game.example.toml")
        }
    };
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let text = std::fs::read_to_string(&manifest_path).unwrap_or_else(|e| {
        fail(&format!("cannot read {}: {e}", manifest_path.display()));
    });
    let manifest: Manifest = toml::from_str(&text).unwrap_or_else(|e| {
        fail(&format!("{} does not parse: {e}", manifest_path.display()));
    });

    if manifest.game.title.trim().is_empty() {
        fail("[game] title must not be empty");
    }
    let id = &manifest.game.id;
    let id_ok = !id.is_empty()
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !id_ok {
        fail("[game] id must be a plain directory name: letters, digits, '.', '-', '_'");
    }

    if !MODELS.contains(&normalize_model(&manifest.machine.model).as_str()) {
        fail(&format!(
            "[machine] model {:?} is not a Copperline machine (one of {})",
            manifest.machine.model,
            MODELS.join(", ")
        ));
    }

    let p = &manifest.payload;
    let kinds = [p.cd.is_some(), p.adf.is_some(), p.run.is_some()];
    if kinds.iter().filter(|k| **k).count() != 1 {
        fail("[payload] must name exactly one of cd, adf, or run");
    }
    if let Some(pin) = &p.sha256 {
        if p.run.is_some() {
            fail("[payload] sha256 applies to file payloads (cd/adf), not run directories");
        }
        if pin.len() != 64 || !pin.chars().all(|c| c.is_ascii_hexdigit()) {
            fail("[payload] sha256 must be 64 hex characters");
        }
    }
    let (kind, file, run_exe, run_args) = if let Some(cd) = &p.cd {
        ("cd", cd.clone(), String::new(), String::new())
    } else if let Some(adf) = &p.adf {
        ("adf", adf.clone(), String::new(), String::new())
    } else {
        let run = p.run.as_ref().expect("guarded above");
        if run.executable.trim().is_empty() {
            fail("[payload] run.executable must not be empty");
        }
        (
            "run",
            run.files.clone(),
            run.executable.clone(),
            run.args.clone(),
        )
    };
    let file_name_only = |what: &str, value: &str| {
        // Exactly one Normal component: a count alone would also accept
        // "." and "..", which resolve to whole directories at runtime.
        let mut components = Path::new(value).components();
        let bare = matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        );
        if !bare {
            fail(&format!(
                "[payload] {what} must be a bare file name beside the binary, not a path: {value:?}"
            ));
        }
    };
    file_name_only(kind, &file);

    let icon = manifest.branding.icon.as_ref().map(|rel| {
        let path = manifest_path.parent().unwrap_or(Path::new(".")).join(rel);
        println!("cargo:rerun-if-changed={}", path.display());
        if !path.is_file() {
            fail(&format!(
                "[branding] icon {} does not exist",
                path.display()
            ));
        }
        path
    });
    let icon_expr = match &icon {
        Some(path) => format!(
            "Some(include_bytes!({:?}))",
            path.canonicalize().unwrap_or_else(|_| path.clone())
        ),
        None => "None".to_string(),
    };
    let pin = p.sha256.as_ref().map(|s| s.to_ascii_lowercase());

    let generated = format!(
        "// Generated by build.rs from {manifest_path:?}; do not edit.\n\
         pub const GAME_TITLE: &str = {title:?};\n\
         pub const GAME_ID: &str = {id:?};\n\
         pub const GAME_VERSION: &str = {version:?};\n\
         pub const MODEL: &str = {model:?};\n\
         pub const CHIP_RAM: Option<&str> = {chip};\n\
         pub const FAST_RAM: Option<&str> = {fast};\n\
         pub const SLOW_RAM: Option<&str> = {slow};\n\
         pub const PAYLOAD_KIND: &str = {kind:?};\n\
         pub const PAYLOAD_FILE: &str = {file:?};\n\
         pub const RUN_EXECUTABLE: &str = {run_exe:?};\n\
         pub const RUN_ARGS: &str = {run_args:?};\n\
         pub const PAYLOAD_SHA256: Option<&str> = {pin};\n\
         pub const SHADER: Option<&str> = {shader};\n\
         pub const BEZEL: Option<&str> = {bezel};\n\
         pub const FULLSCREEN: Option<bool> = {fullscreen:?};\n\
         pub const SAVE_STATES: bool = {save_states:?};\n\
         pub const ICON_PNG: Option<&[u8]> = {icon_expr};\n",
        title = manifest.game.title,
        id = manifest.game.id,
        version = manifest.game.version.as_deref().unwrap_or("1.0"),
        model = manifest.machine.model,
        chip = opt_str(&manifest.machine.chip),
        fast = opt_str(&manifest.machine.fast),
        slow = opt_str(&manifest.machine.slow),
        pin = opt_str(&pin),
        shader = opt_str(&manifest.display.shader),
        bezel = opt_str(&manifest.display.bezel),
        fullscreen = manifest.display.fullscreen,
        save_states = manifest.features.save_states,
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets this")).join("baked.rs");
    std::fs::write(&out, generated).expect("writing the baked manifest module");
}
