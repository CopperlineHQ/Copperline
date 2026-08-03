use std::path::Path;
use std::process::Command;

fn main() {
    let package_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let git_dir = git_output(["rev-parse", "--git-dir"]);
    let git_common_dir = git_output(["rev-parse", "--git-common-dir"]);
    if let Some(git_dir) = git_dir.as_deref() {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }
    if let (Some(git_dir), Some(git_common_dir)) = (git_dir.as_deref(), git_common_dir.as_deref()) {
        if let Some(head_ref) = current_head_ref(git_dir) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
            println!("cargo:rerun-if-changed={git_common_dir}/{head_ref}");
        }
    }
    if let Some(git_common_dir) = git_common_dir.as_deref() {
        println!("cargo:rerun-if-changed={git_common_dir}/packed-refs");
        println!("cargo:rerun-if-changed={git_common_dir}/refs/tags");
    }

    let display_version = if exact_tagged_head() {
        package_version
    } else if let Some(short_hash) = git_output(["rev-parse", "--short=8", "HEAD"]) {
        format!("{package_version}+g{short_hash}")
    } else {
        package_version
    };

    println!("cargo:rustc-env=COPPERLINE_DISPLAY_VERSION={display_version}");

    set_windows_main_thread_stack();
    build_floppybridge();
    build_munt();
}

/// Compile the vendored FloppyBridge into the emulator, so a build produces a
/// binary that can drive a real floppy drive with nothing else to install.
///
/// Upstream ships this as a shared library to be loaded at run time. Copperline
/// links it instead: a user should not have to fetch a second file to use a
/// feature the build claims to have, and keeping the two in one binary removes
/// a whole class of "which copy is it loading" problems. Updating upstream is a
/// maintainer's job, and a wholesale copy -- see vendor/floppybridge/README.md.
fn build_floppybridge() {
    if std::env::var_os("CARGO_FEATURE_FLOPPYBRIDGE").is_none() {
        return;
    }
    let dir = Path::new("vendor/floppybridge/src");
    // Upstream's own source list, minus two files: `floppybridge_lib.cpp` is
    // the client-side loader for the shared build, which linking directly
    // replaces, and `ADFBridge.cpp` backs a bridge onto an ADF file, which is
    // what Copperline's own image path already does.
    const SOURCES: [&str; 12] = [
        "ArduinoFloppyBridge.cpp",
        "ArduinoInterface.cpp",
        "CommonBridgeTemplate.cpp",
        "FloppyBridge.cpp",
        "GreaseWeazleBridge.cpp",
        "GreaseWeazleInterface.cpp",
        "RotationExtractor.cpp",
        "SerialIO.cpp",
        "SuperCardProBridge.cpp",
        "SuperCardProInterface.cpp",
        "ftdi.cpp",
        "pll.cpp",
    ];
    println!("cargo:rerun-if-changed=vendor/floppybridge/src");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(dir)
        // Excludes the WinUAE dialogs, their resource script, and the update
        // check, none of which Copperline uses; see the vendor README.
        .define("FLOPPYBRIDGE_NO_GUI", None)
        .warnings(false);
    if target_os == "windows" {
        // `windows.h` defines `min` and `max` as macros unless told not to,
        // which turns every `std::min(` in the sources into `std::(`. Upstream
        // builds from a Visual Studio project that sets this; nothing here uses
        // the macros.
        build.define("NOMINMAX", None);
    }
    if target_env == "msvc" {
        // The sources use the standard library's containers and threads, so
        // they need real unwinding. MSVC does not enable it by default and
        // warns that any exception would terminate instead.
        build.flag("/EHsc");
    }
    for source in SOURCES {
        build.file(dir.join(source));
    }
    build.compile("floppybridge");

    // What the sources need beyond libc. Windows names its own through
    // `#pragma comment(lib, ...)`, so MSVC picks those up without help.
    if target_os != "windows" {
        println!("cargo:rustc-link-lib=dylib=dl");
    }
    // The C++ runtime the bridge's threads and containers are built against.
    match target_os.as_str() {
        "macos" | "ios" => println!("cargo:rustc-link-lib=dylib=c++"),
        "windows" => {}
        _ => println!("cargo:rustc-link-lib=dylib=stdc++"),
    }
}

/// Give the Windows binaries the same ~8 MiB main-thread stack that Linux and
/// macOS provide by default; the MSVC linker otherwise reserves only 1 MiB.
///
/// The winit event loop must run on the main thread, and the configuration
/// screen's Run boots the machine from inside that event-loop callback -- so the
/// machine build, the render-state rebuild, and the first present all run deep in
/// the OS message-pump stack. That bounded-but-substantial work overflows the
/// 1 MiB default (a silent exit / STATUS_STACK_OVERFLOW when the user clicks Run),
/// while it fits comfortably in the 8 MiB the other platforms already give. Scoped
/// to binary targets and, via the target cfg, to Windows MSVC builds only.
fn set_windows_main_thread_stack() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        const STACK_BYTES: usize = 8 * 1024 * 1024;
        println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
    }
}

fn current_head_ref(git_dir: &str) -> Option<String> {
    let head = std::fs::read_to_string(Path::new(git_dir).join("HEAD")).ok()?;
    head.strip_prefix("ref: ").map(|s| s.trim().to_string())
}

fn exact_tagged_head() -> bool {
    git_output(["describe", "--tags", "--exact-match", "HEAD"]).is_some()
}

fn git_output<const N: usize>(args: [&str; N]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Compile Munt's mt32emu, the MT-32 synthesiser engine, into the
/// emulator. Vendored whole -- keeping it in step with upstream is the
/// maintainer's job -- see vendor/munt/README.md.
fn build_munt() {
    if std::env::var_os("CARGO_FEATURE_MT32").is_none() {
        return;
    }
    let dir = Path::new("vendor/munt/src");
    // Upstream's own library source list. The optional resampler adapters are
    // not among them: MT32EMU_WITH_INTERNAL_RESAMPLER below picks the built-in
    // one, which is what keeps this free of external dependencies.
    const SOURCES: [&str; 29] = [
        "Analog.cpp",
        "BReverbModel.cpp",
        "Display.cpp",
        "File.cpp",
        "FileStream.cpp",
        "LA32FloatWaveGenerator.cpp",
        "LA32Ramp.cpp",
        "LA32WaveGenerator.cpp",
        "MidiStreamParser.cpp",
        "Part.cpp",
        "Partial.cpp",
        "PartialManager.cpp",
        "Poly.cpp",
        "ROMInfo.cpp",
        "SampleRateConverter.cpp",
        "Synth.cpp",
        "TVA.cpp",
        "TVF.cpp",
        "TVP.cpp",
        "Tables.cpp",
        "VersionTagging.cpp",
        "c_interface/c_interface.cpp",
        "sha1/sha1.cpp",
        "srchelper/InternalResampler.cpp",
        "srchelper/srctools/src/FIRResampler.cpp",
        "srchelper/srctools/src/IIR2xResampler.cpp",
        "srchelper/srctools/src/LinearResampler.cpp",
        "srchelper/srctools/src/ResamplerModel.cpp",
        "srchelper/srctools/src/SincResampler.cpp",
    ];
    println!("cargo:rerun-if-changed=vendor/munt/src");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .include(dir)
        // The built-in sample-rate converter, so the engine renders straight
        // at the mixer's rate with nothing else to link against.
        .define("MT32EMU_WITH_INTERNAL_RESAMPLER", "1")
        .warnings(false);
    if target_env == "msvc" {
        // The sources use the standard library's containers, so they need
        // real unwinding; MSVC does not enable it by default.
        build.flag("/EHsc");
        // No standard is asked for here: MSVC has no C++11 setting -- its
        // lowest is C++14, which its default already meets -- so naming one
        // earns a "command line warning D9002" from cl for every source.
    } else {
        build.std("c++11");
    }
    for source in SOURCES {
        build.file(dir.join(source));
    }
    // Copperline's own shim, which formats the engine's diagnostics where
    // the compiler knows what a va_list is.
    println!("cargo:rerun-if-changed=src/mt32/print_debug.cpp");
    build.file("src/mt32/print_debug.cpp");
    build.compile("mt32emu");

    // The C++ runtime the engine's containers are built against.
    match target_os.as_str() {
        "macos" | "ios" => println!("cargo:rustc-link-lib=dylib=c++"),
        "windows" => {}
        _ => println!("cargo:rustc-link-lib=dylib=stdc++"),
    }
}
