//! Headless debugger transparency: arming `COPPERLINE_DBG_*`-style
//! observation must not move the emulated timeline
//! (docs/debugger/headless.md, "Timeline transparency").
//!
//! The headless debugger is a pure observer. Its per-instruction hooks read
//! machine state through side-effect-free peeks, never bill bus time, and
//! only write to the host log. A run with breakpoints, watchpoints, exception
//! catches, and an instruction trace armed must therefore retire the same
//! instructions at the same colour clocks as an undebugged run, write the
//! same chip RAM, and render the same frames -- so an investigation can add
//! instrumentation without changing what it is investigating.
//!
//! Two machines boot the bundled AROS ROM side by side from the same
//! configuration. One is armed with every hook the environment could arm
//! (chosen so they all fire during the boot), the other is left alone. Both
//! are stepped frame by frame and fingerprinted after every frame; the first
//! mismatch names the frame.
//!
//! Asset-free: the bundled AROS ROM boots with an empty DF0. The guest clock
//! is pinned with a fixed RTC seed so the two builds agree.
//!
//! Release-only, like tests/savestate_roundtrip.rs: a debug-build emulator is
//! far too slow for a multi-second emulated boot.
//!
//! ```sh
//! cargo test --release --test debugger_transparency -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::Instant;

use copperline::audio::NullSink;
use copperline::config::Config;
use copperline::debugger::{Debugger, Watch};
use copperline::emulator::{build_machine, Emulator};
use copperline::video::{bitplane, FB_WIDTH, MAX_CANVAS_PIXELS};

/// Emulated seconds to run both machines. Long enough to cover the AROS
/// bootstrap handing control up and the guest screen coming live.
const RUN_SECS: f64 = 12.0;

/// Fixed power-on RTC seed (Unix seconds).
const RTC_SEED: u64 = 1_111_111_109;

/// Level-3 autovector (VERTB): vector 24 + 3. Caught so the exception hook
/// fires on every frame.
const VERTB_VECTOR: u16 = 27;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pin_bundled_aros() {
    std::env::set_var("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"));
}

fn make_config() -> anyhow::Result<Config> {
    let mut cfg = Config {
        rtc_seed_unix: Some(RTC_SEED),
        ..Config::default()
    };
    copperline::config::resolve_bundled_rom(&mut cfg)?;
    Ok(cfg)
}

fn build() -> anyhow::Result<Emulator> {
    build_machine(&make_config()?, Box::new(NullSink), false, false)
}

/// Every environment-armable hook at once, tuned so each one fires during
/// the boot: a watch over the low chip-RAM vector/ExecBase area the guest
/// rewrites constantly, a PC breakpoint on the reset vector's first
/// instruction, the VERTB exception catch (every frame), the exec Alert()
/// trap arming, a memory dump on every hit, and an instruction trace over a
/// short window. The hit budget is unbounded so the hooks never retire.
fn armed_debugger(emu: &Emulator) -> Debugger {
    let mut dbg = Debugger::new(emu.machine.ui_addr_mask());
    dbg.watches.push(Watch {
        addr: 0x0000_0000,
        len: 0x200,
    });
    dbg.breakpoints
        .push(emu.machine.pc() & emu.machine.ui_addr_mask());
    dbg.catches.push(VERTB_VECTOR);
    dbg.catch_alert = true;
    dbg.dumps.push((0x0000_0004, 4));
    dbg.trace = true;
    dbg.after_secs = 0.0;
    dbg.until_secs = f64::INFINITY;
    dbg.max_hits = u64::MAX;
    dbg
}

fn fnv1a64_from(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Everything two runs must agree on at one emulated instant: the timeline
/// position, the CPU's place in the program, the rendered frame, and the
/// whole of chip RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    cck: u64,
    frames: u64,
    pc: u32,
    fb_hash: u64,
    chip_hash: u64,
}

impl Fingerprint {
    /// Capture via the side-effect-free display path, exactly as the control
    /// protocol's `capture.digest` does.
    fn capture(emu: &Emulator) -> Self {
        let input = bitplane::RenderInput::from_bus(emu.bus());
        let mut fb = vec![0u32; MAX_CANVAS_PIXELS];
        bitplane::render_from_input(&input, &mut fb);
        let lines = emu.bus().frame_geometry().visible_lines;
        let width = FB_WIDTH * emu.bus().frame_canvas_scale();
        let mut fb_hash = 0xcbf2_9ce4_8422_2325;
        for px in &fb[..width * lines] {
            fb_hash = fnv1a64_from(fb_hash, &px.to_le_bytes());
        }
        let chip_hash = fnv1a64_from(0xcbf2_9ce4_8422_2325, emu.bus().mem.chip_ram.as_slice());
        Fingerprint {
            cck: emu.bus().emulated_cck(),
            frames: emu.bus().emulated_frames(),
            pc: emu.machine.pc(),
            fb_hash,
            chip_hash,
        }
    }
}

/// Step both machines one frame at a time to `RUN_SECS`, holding the armed
/// machine to the plain machine's fingerprint after every frame.
fn run_side_by_side(plain: &mut Emulator, armed: &mut Emulator) -> anyhow::Result<u64> {
    let mut frames = 0u64;
    let mut progressed = false;
    let mut last_chip = Fingerprint::capture(plain).chip_hash;
    while plain.bus().emulated_seconds() < RUN_SECS {
        plain.step_frame()?;
        armed.step_frame()?;
        frames += 1;
        let want = Fingerprint::capture(plain);
        let got = Fingerprint::capture(armed);
        assert_eq!(
            want, got,
            "the armed headless debugger moved the timeline {frames} frame(s) in:\n  \
             plain = {want:?}\n  armed = {got:?}"
        );
        if want.chip_hash != last_chip {
            progressed = true;
            last_chip = want.chip_hash;
        }
    }
    assert!(
        progressed,
        "chip RAM never changed across the run; the comparison would be vacuous"
    );
    Ok(frames)
}

/// Arming breakpoints, watchpoints, exception catches, Alert() catching,
/// memory dumps and an instruction trace leaves every frame of the boot
/// byte-identical to an undebugged run.
#[test]
fn armed_headless_debugger_leaves_the_timeline_unchanged() {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping debugger transparency; run with --release \
             (a debug emulator is too slow for the emulated boot)"
        );
        return;
    }
    pin_bundled_aros();

    let started = Instant::now();
    let mut plain = build().expect("plain machine");
    let mut armed = build().expect("armed machine");
    let dbg = armed_debugger(&armed);
    armed.machine.arm_headless_debugger(Some(dbg));
    assert!(armed.machine.headless_debugger_armed());
    assert!(!plain.machine.headless_debugger_armed());

    let frames = run_side_by_side(&mut plain, &mut armed).expect("side-by-side run");
    println!(
        "  {frames} frames ({RUN_SECS}s emulated) byte-identical with the debugger armed, \
         {:.1}s wall",
        started.elapsed().as_secs_f64()
    );
}
