//! HRTMon freezer cartridge: pressing the button on a running machine enters
//! the monitor (docs/guide/configuration.md "[cartridge]",
//! docs/internals/peripherals.md "Freezer cartridge").
//!
//! Asset-free: the bundled AROS ROM boots with an empty DF0 and the bundled
//! HRTMon image (assets/hrtmon, built by hrtmon-rom/build.sh) is the
//! cartridge. The guest clock is pinned with a fixed RTC seed so every run
//! is the same on every host.
//!
//! Two routes to the same freeze are covered: the library call the menu row,
//! the shortcut and the control protocol's `cartridge.freeze` all land on,
//! and the `--freeze-after` flag through the real binary. Both are held to
//! the monitor's own evidence -- the `entered` flag it sets in its
//! configuration block and the screen it paints in the block's colours --
//! and the library route also checks that a state taken inside the monitor
//! resumes inside it.
//!
//! Release-only, like tests/savestate_roundtrip.rs: a debug-build emulator
//! is far too slow for the emulated boot.
//!
//! ```sh
//! cargo test --release --test hrtmon_freeze -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use copperline::audio::NullSink;
use copperline::cartridge::{CartridgeModel, CFG_ENTERED, HRTMON_BASE, HRTMON_ENTRY_OFFSET};
use copperline::chipset::denise::rgb12_to_rgba8;
use copperline::config::{CartridgeConfig, Config, BUNDLED_HRTMON_ROM};
use copperline::emulator::{build_machine, Emulator};
use copperline::video::{bitplane, FB_WIDTH, MAX_CANVAS_PIXELS};

/// When the button is pressed: past the AROS bootstrap (~11s on the
/// boot-time-optimized bundled ROM), with the guest idle on its screen.
const FREEZE_SECS: f64 = 14.0;
/// When the monitor is looked at: it installs itself on the first entry
/// (a CIA-timed IDE probe included on machines with an IDE port; the A500
/// shape here has none) and paints its screen well within this.
const CHECK_SECS: f64 = 17.0;
/// Fixed power-on RTC seed (Unix seconds), as in tests/savestate_roundtrip.rs.
const RTC_SEED: u64 = 1_111_111_109;
/// The monitor's background: COLOR00 from the configuration block the host
/// writes (`$005A`, dark blue).
const MONITOR_BACKGROUND: u16 = 0x005A;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Pin where the bundled AROS ROM pair and the bundled HRTMon image are
/// found, so neither the test process nor the binary it launches depends on
/// the working directory or an install layout.
fn pin_bundled_roms() {
    std::env::set_var("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"));
    std::env::set_var("COPPERLINE_HRTMON_DIR", repo_root().join("assets/hrtmon"));
}

/// A deterministic A500-shape config: bundled AROS, empty DF0, fixed RTC,
/// the bundled HRTMon cartridge. The bundled-ROM sentinels are resolved
/// here (the host's job in `main.rs`) so `build_machine` sees real paths.
fn make_config() -> anyhow::Result<Config> {
    let mut cfg = Config {
        rtc_seed_unix: Some(RTC_SEED),
        cartridge: CartridgeConfig {
            model: Some(CartridgeModel::Hrtmon),
            rom: Some(PathBuf::from(BUNDLED_HRTMON_ROM)),
        },
        ..Config::default()
    };
    copperline::config::resolve_bundled_rom(&mut cfg)?;
    Ok(cfg)
}

fn build() -> anyhow::Result<Emulator> {
    build_machine(&make_config()?, Box::new(NullSink), false, false)
}

fn run_until(emu: &mut Emulator, secs: f64) -> anyhow::Result<()> {
    while emu.bus().emulated_seconds() < secs {
        emu.step_frame()?;
    }
    Ok(())
}

/// The colour most of the visible frame shows, and its share of it, from
/// the side-effect-free display path (as `capture.digest` renders).
fn dominant_colour(emu: &Emulator) -> (u32, f64) {
    let input = bitplane::RenderInput::from_bus(emu.bus());
    let mut fb = vec![0u32; MAX_CANVAS_PIXELS];
    bitplane::render_from_input(&input, &mut fb);
    let lines = emu.bus().frame_geometry().visible_lines;
    let width = FB_WIDTH * emu.bus().frame_canvas_scale();
    let pixels = &fb[..width * lines];
    let mut counts = std::collections::HashMap::new();
    for px in pixels {
        *counts.entry(px & 0x00FF_FFFF).or_insert(0usize) += 1;
    }
    let (colour, count) = counts.into_iter().max_by_key(|&(_, n)| n).unwrap();
    (colour, count as f64 / pixels.len() as f64)
}

fn monitor_background() -> u32 {
    rgb12_to_rgba8(MONITOR_BACKGROUND) & 0x00FF_FFFF
}

fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "copperline-hrtmon-freeze-{}-{name}",
        std::process::id()
    ))
}

fn skip_in_debug() -> bool {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping hrtmon freeze; run with --release \
             (a debug emulator is too slow for the emulated boot)"
        );
        return true;
    }
    false
}

/// The button through the library: the level-7 vector names the cartridge
/// entry, the monitor sets `entered` and paints its screen, and a state
/// taken inside the monitor resumes inside it.
#[test]
fn freeze_enters_the_monitor_and_a_state_resumes_inside_it() {
    if skip_in_debug() {
        return;
    }
    pin_bundled_roms();

    let mut emu = build().expect("build machine");
    run_until(&mut emu, FREEZE_SECS).expect("boot");
    let cartridge = emu.cartridge().expect("the config fits the cartridge");
    assert_eq!(cartridge.model(), CartridgeModel::Hrtmon);
    assert_eq!(
        cartridge.version(),
        Some((2, 39)),
        "the bundled image is 2.39"
    );
    assert!(!cartridge.entered());
    let before = dominant_colour(&emu);
    assert_ne!(
        before.0,
        monitor_background(),
        "AROS is on screen before the freeze"
    );

    let entry = emu.cartridge_freeze().expect("freeze");
    assert_eq!(entry, HRTMON_BASE + HRTMON_ENTRY_OFFSET);
    run_until(&mut emu, CHECK_SECS).expect("run into the monitor");

    let cartridge = emu.cartridge().unwrap();
    assert!(
        cartridge.entered(),
        "the monitor sets bit 0 of `entered` (+{CFG_ENTERED}) on entry"
    );
    assert_eq!(cartridge.freezes(), 1);
    assert!(!cartridge.nmi_pending(), "the interrupt was taken");
    let (colour, share) = dominant_colour(&emu);
    assert_eq!(
        colour,
        monitor_background(),
        "the monitor's screen is up in the block's COLOR00"
    );
    assert!(share > 0.5, "the background covers the frame ({share:.2})");

    // A state taken inside the monitor: the bank (the monitor's variables,
    // stack and screen) travels with it, so the resumed machine is inside
    // the monitor too, and stays there.
    let blob = emu.save_state_bytes().expect("save state");
    let mut resumed = build().expect("build a fresh machine");
    resumed.load_state_bytes(&blob).expect("load state");
    let restored = resumed.cartridge().expect("the state fits the cartridge");
    assert!(restored.entered());
    assert_eq!(restored.bank(), emu.cartridge().unwrap().bank());
    for _ in 0..3 {
        resumed.step_frame().expect("step");
        emu.step_frame().expect("step");
    }
    assert!(resumed.cartridge().unwrap().entered());
    assert_eq!(dominant_colour(&resumed), dominant_colour(&emu));
}

/// The button through the binary: `--freeze-after` presses it at the
/// emulated instant, the state saved afterwards is inside the monitor, and
/// the screenshot shows its screen.
#[test]
fn freeze_after_flag_enters_the_monitor_headless() {
    if skip_in_debug() {
        return;
    }
    pin_bundled_roms();
    let state = scratch_path("flag.clstate");
    let shot = scratch_path("flag.png");

    let status = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(repo_root())
        .args([
            "--factory",
            "--noaudio",
            "--rtc-time",
            &RTC_SEED.to_string(),
        ])
        .args(["--cartridge", "hrtmon"])
        .args(["--freeze-after", &FREEZE_SECS.to_string()])
        .arg("--save-state-after")
        .arg(CHECK_SECS.to_string())
        .arg(&state)
        .arg("--screenshot-after")
        .arg(CHECK_SECS.to_string())
        .arg(&shot)
        .status()
        .expect("run copperline");
    assert!(status.success(), "copperline exited with {status}");

    let mut emu = build().expect("build machine");
    emu.load_state(&state)
        .expect("load the state the run saved");
    let cartridge = emu.cartridge().expect("the state carries the cartridge");
    assert!(cartridge.entered(), "--freeze-after entered the monitor");
    assert_eq!(cartridge.freezes(), 1);

    let (colour, share) = dominant_png_colour(&shot);
    let want = monitor_background();
    assert_eq!(
        colour,
        [want as u8, (want >> 8) as u8, (want >> 16) as u8],
        "the screenshot shows the monitor's screen"
    );
    assert!(
        share > 0.5,
        "the background covers the screenshot ({share:.2})"
    );

    let _ = std::fs::remove_file(&state);
    let _ = std::fs::remove_file(&shot);
}

/// The RGB triple most of a PNG shows, and its share of it.
fn dominant_png_colour(path: &Path) -> ([u8; 3], f64) {
    let file = std::fs::File::open(path).expect("open screenshot");
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().expect("png header");
    let mut buf = vec![0u8; reader.output_buffer_size().expect("png size")];
    let info = reader.next_frame(&mut buf).expect("png frame");
    let stride = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => panic!("unexpected screenshot colour type {other:?}"),
    };
    let pixels = buf[..info.buffer_size()].chunks_exact(stride);
    let total = pixels.len();
    let mut counts = std::collections::HashMap::new();
    for px in pixels {
        *counts.entry([px[0], px[1], px[2]]).or_insert(0usize) += 1;
    }
    let (colour, count) = counts.into_iter().max_by_key(|&(_, n)| n).unwrap();
    (colour, count as f64 / total as f64)
}
