//! Save-state resume regression: a resumed run must be byte-identical to an
//! uninterrupted one (docs/guide/headless.md, "Save states").
//!
//! The emulator core is deterministic and independent of wall-clock pacing,
//! so two machines that reach the same emulated instant through different
//! histories -- one stepped straight through, one restored at T from a state
//! written by another machine -- must hold identical memory and produce
//! identical frames from then on. Any difference means some machine state is
//! missing from (or corrupted by) the save-state format.
//!
//! One deliberate exception shapes the comparisons below: the state loader
//! launders the per-frame render-capture buffers
//! (`Bus::reset_transient_video_after_state_load`, see the module comment in
//! src/savestate.rs), so the FIRST post-resume frame re-renders from rebuilt
//! capture state and its pixels are not comparable; its machine state is.
//! From the next frame wrap on, the capture buffers rebuild identically and
//! every fingerprint field must match. The checkpoints reflect that: frame
//! +1 compares timeline and chip RAM, later frames compare everything.
//!
//! Asset-free: the bundled AROS ROM boots with an empty DF0, no disk image
//! needed. The guest clock is pinned with a fixed RTC seed so separate
//! machine builds agree (an unseeded clock reads the host wall clock).
//!
//! Fingerprints follow the control protocol's `capture.digest` recipe
//! (src/control/exec.rs): FNV-1a over the side-effect-free display-path
//! framebuffer chained with FNV-1a over chip RAM. The render input is built
//! fresh per capture (`RenderInput::from_bus`) so the digest is a pure
//! function of machine state, not of this thread's capture history --
//! `render_display_only` would reuse a thread-local input that refills
//! incrementally across whatever machines were captured before.
//!
//! Release-only, like tests/probe_golden.rs: a debug-build emulator is far
//! too slow for multi-second emulated boots.
//!
//! ```sh
//! cargo test --release --test savestate_roundtrip -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use copperline::audio::NullSink;
use copperline::config::Config;
use copperline::emulator::{build_machine, Emulator};
use copperline::video::{bitplane, FB_WIDTH, MAX_CANVAS_PIXELS};

/// Emulated seconds before the split point. Past the AROS bootstrap handing
/// control up (~11s on the boot-time-optimized bundled ROM), with the guest
/// screen live.
const SPLIT_SECS: f64 = 12.0;

/// Post-split checkpoints, in whole frames from the split point. The first
/// entry is the critical one: the first frame completed after the resume,
/// where only machine state is comparable (see the module comment). The rest
/// spread over ~3 emulated seconds (PAL, ~50 frames/s) and compare fully.
const CHECKPOINT_FRAMES: [u64; 4] = [1, 25, 75, 150];

/// Fixed power-on RTC seed (Unix seconds) so every machine build starts the
/// guest clock at the same instant instead of the host wall clock.
const RTC_SEED: u64 = 1_111_111_109;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Pin where the bundled AROS ROM pair is found, so the test does not depend
/// on the process's working directory or install layout.
fn pin_bundled_aros() {
    std::env::set_var("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"));
}

/// A deterministic A500-shape config: bundled AROS, empty DF0, fixed RTC.
/// The bundled-ROM sentinel is resolved here (the host's job in `main.rs`)
/// so `build_machine` sees real ROM paths.
fn make_config() -> anyhow::Result<Config> {
    let mut cfg = Config {
        rtc_seed_unix: Some(RTC_SEED),
        ..Config::default()
    };
    copperline::config::resolve_bundled_rom(&mut cfg)?;
    Ok(cfg)
}

/// A fresh machine, unpaced (headless warp: determinism does not depend on
/// wall-clock pacing).
fn build() -> anyhow::Result<Emulator> {
    build_machine(&make_config()?, Box::new(NullSink), false, false)
}

/// Step whole frames until the machine has completed `target` frames.
fn run_to_frame(emu: &mut Emulator, target: u64) -> anyhow::Result<()> {
    while emu.bus().emulated_frames() < target {
        emu.step_frame()?;
    }
    Ok(())
}

/// Boot a fresh machine and step whole frames to the split point.
fn boot_to_split() -> anyhow::Result<(Emulator, u64)> {
    let mut emu = build()?;
    run_to_frame(&mut emu, 0)?;
    while emu.bus().emulated_seconds() < SPLIT_SECS {
        emu.step_frame()?;
    }
    let split_frame = emu.bus().emulated_frames();
    Ok((emu, split_frame))
}

fn fnv1a64_from(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Everything a byte-identical run must agree on at one emulated instant:
/// the timeline position, the rendered frame, and the whole of chip RAM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    seconds_bits: u64,
    frames: u64,
    fb_hash: u64,
    chip_hash: u64,
}

impl Fingerprint {
    /// Capture via the side-effect-free display path, exactly as the control
    /// protocol's `capture.digest` does, so observation cannot change what is
    /// being compared.
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
            seconds_bits: emu.bus().emulated_seconds().to_bits(),
            frames: emu.bus().emulated_frames(),
            fb_hash,
            chip_hash,
        }
    }

    /// The machine-state half of the fingerprint: everything that must match
    /// even on the first post-resume frame, whose pixels are not comparable
    /// because the loader launders the render-capture buffers by design.
    fn machine_state(self) -> (u64, u64, u64) {
        (self.seconds_bits, self.frames, self.chip_hash)
    }
}

fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "copperline-savestate-roundtrip-{}-{name}",
        std::process::id()
    ))
}

/// Step to each checkpoint frame past `split_frame`, fingerprinting on the
/// way. Callers hold the uninterrupted run's prints as the record to match.
fn run_checkpoints(emu: &mut Emulator, split_frame: u64) -> anyhow::Result<Vec<Fingerprint>> {
    let mut prints = Vec::with_capacity(CHECKPOINT_FRAMES.len());
    for offset in CHECKPOINT_FRAMES {
        run_to_frame(emu, split_frame + offset)?;
        prints.push(Fingerprint::capture(emu));
    }
    Ok(prints)
}

/// Hold a resumed history to the uninterrupted run's record. Frame +1 must
/// match on machine state only (laundered capture buffers); every later
/// checkpoint must match exactly.
fn assert_checkpoints_match(label: &str, expected: &[Fingerprint], actual: &[Fingerprint]) {
    let (want, got) = (&expected[0], &actual[0]);
    assert_eq!(
        want.machine_state(),
        got.machine_state(),
        "{label}: machine state diverged on the FIRST post-resume frame:\n  \
         uninterrupted = {want:?}\n  resumed       = {got:?}"
    );
    for i in 1..CHECKPOINT_FRAMES.len() {
        assert_eq!(
            expected[i], actual[i],
            "{label}: resumed run diverged from the uninterrupted run {} \
             frame(s) after the split:\n  uninterrupted = {:?}\n  {label:<13}= {:?}",
            CHECKPOINT_FRAMES[i], expected[i], actual[i],
        );
    }
}

/// The uninterrupted history: boot, step straight through the split point and
/// every checkpoint, fingerprinting on the way past. The state written at the
/// last checkpoint (`final_state`) is the byte-for-byte record a resumed run
/// must reproduce.
fn uninterrupted_run(final_state: &Path) -> anyhow::Result<(Fingerprint, Vec<Fingerprint>)> {
    let started = Instant::now();
    let (mut emu, split_frame) = boot_to_split()?;
    let at_split = Fingerprint::capture(&emu);
    println!(
        "  uninterrupted: reached T={SPLIT_SECS}s (frame {split_frame}) in {:.1}s wall",
        started.elapsed().as_secs_f64()
    );

    let t1 = Instant::now();
    let checkpoints = run_checkpoints(&mut emu, split_frame)?;
    let last = checkpoints[checkpoints.len() - 1];
    println!(
        "  uninterrupted: ran {} checkpoint frames in {:.1}s wall \
         (last fb {:016x}, chip {:016x})",
        CHECKPOINT_FRAMES[CHECKPOINT_FRAMES.len() - 1],
        t1.elapsed().as_secs_f64(),
        last.fb_hash,
        last.chip_hash,
    );
    emu.save_state(final_state)?;
    Ok((at_split, checkpoints))
}

/// A resumed run's state file at the last checkpoint must be byte-identical
/// to the uninterrupted run's: the fingerprints prove the timeline agrees,
/// this proves nothing host-side leaked into (or was laundered out of) the
/// serialized machine along the way. A single differing byte is a field
/// that belongs out of the layout (host diagnostics) or a field the loader
/// fails to carry across.
fn assert_final_states_identical(label: &str, expected: &Path, actual: &Path) {
    let want = std::fs::read(expected).expect("read the uninterrupted run's final state");
    let got = std::fs::read(actual).expect("read the resumed run's final state");
    if want == got {
        return;
    }
    let first = want
        .iter()
        .zip(got.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(want.len().min(got.len()));
    panic!(
        "{label}: the final state file differs from the uninterrupted run's \
         (sizes {} vs {}, first difference at byte {first})",
        want.len(),
        got.len()
    );
}

/// The resumed history: a second machine boots independently to the split
/// point (proving cross-build determinism of the split state itself), saves,
/// hands the state file to a THIRD freshly built machine, which is then held
/// to the uninterrupted run's checkpoint-by-checkpoint record.
fn resumed_run(
    state_path: &Path,
    at_split: &Fingerprint,
    expected: &[Fingerprint],
    final_state: &Path,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let (mut emu, split_frame) = boot_to_split()?;
    println!(
        "  resumed: second build reached T={SPLIT_SECS}s in {:.1}s wall",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(
        Fingerprint::capture(&emu),
        *at_split,
        "two independent boots disagree at the split point; the roundtrip \
         comparison below would not be about save states"
    );

    // Call between frames only, as documented on save_state.
    emu.save_state(state_path)?;

    // A fresh construction loads the state; the state carries its own
    // machine, so nothing about the loader build's early life can leak in.
    let mut resumed = build()?;
    let outcome = resumed.load_state(state_path)?;
    println!(
        "  resumed: state loaded into a fresh machine (reconfigured={}, {})",
        outcome.reconfigured, outcome.summary
    );
    assert_eq!(
        Fingerprint::capture(&resumed).frames,
        at_split.frames,
        "restored timeline is not at the split point"
    );

    let t1 = Instant::now();
    let actual = run_checkpoints(&mut resumed, split_frame)?;
    println!(
        "  resumed: checkpoints re-run in {:.1}s wall",
        t1.elapsed().as_secs_f64()
    );
    assert_checkpoints_match("resumed", expected, &actual);
    let resumed_final = scratch_path("final-resumed.clstate");
    resumed.save_state(&resumed_final)?;
    assert_final_states_identical("resumed", final_state, &resumed_final);
    let _ = std::fs::remove_file(&resumed_final);
    Ok(())
}

/// Same property as `resume_matches_uninterrupted_run`, but the loading
/// machine is built through a different construction path: a TOML config
/// parsed and validated through `Config::load_raw`/`try_into` rather than
/// `Config::default()`. The state must restore identically regardless of how
/// the host built its machine.
fn resumed_run_alternate_construction(
    state_path: &Path,
    expected: &[Fingerprint],
) -> anyhow::Result<()> {
    let cfg_path = scratch_path("alt.toml");
    std::fs::write(
        &cfg_path,
        format!("rom = \"<bundled-aros>\"\n[machine]\nrtc_time = {RTC_SEED}\n"),
    )
    .expect("write alternate-construction config");
    let raw = Config::load_raw(Some(&cfg_path), &Default::default())?;
    let mut cfg: Config = raw.try_into()?;
    assert_eq!(cfg.rtc_seed_unix, Some(RTC_SEED));
    copperline::config::resolve_bundled_rom(&mut cfg)?;

    let mut emu = build_machine(&cfg, Box::new(NullSink), false, false)?;
    let outcome = emu.load_state(state_path)?;
    println!(
        "  alternate construction: state loaded (reconfigured={})",
        outcome.reconfigured
    );
    // The state carries its own timeline; the checkpoints are relative to it.
    let split_frame = emu.bus().emulated_frames();
    let actual = run_checkpoints(&mut emu, split_frame)?;
    let _ = std::fs::remove_file(&cfg_path);
    assert_checkpoints_match("alternate", expected, &actual);
    Ok(())
}

/// The core property: resume is byte-identical to an uninterrupted run --
/// identical machine state from the first post-resume frame and identical
/// rendered output from the next frame wrap on -- including when the loading
/// machine was built through a different construction path.
#[test]
fn resume_matches_uninterrupted_run() {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping savestate roundtrip; run with --release \
             (a debug emulator is too slow for the emulated boot)"
        );
        return;
    }
    pin_bundled_aros();

    println!("run A: uninterrupted");
    let final_state = scratch_path("final.clstate");
    let (at_split, checkpoints) = uninterrupted_run(&final_state).expect("uninterrupted run");

    // Sanity: the machine actually advanced across the checkpoint span. All
    // equal would make the comparison vacuous.
    assert!(
        checkpoints
            .iter()
            .any(|fp| fp.chip_hash != at_split.chip_hash),
        "no observable progress after the split point; the comparison would be vacuous"
    );

    println!("run B: save at T, resume in a fresh machine");
    let state_path = scratch_path("split.clstate");
    resumed_run(&state_path, &at_split, &checkpoints, &final_state).expect("resumed run");

    println!("run C: same state into a differently constructed machine");
    resumed_run_alternate_construction(&state_path, &checkpoints).expect("alternate run");

    let _ = std::fs::remove_file(&state_path);
    let _ = std::fs::remove_file(&final_state);
}

/// Two independent builds stepped to the same emulated instant must already
/// agree before any save state is involved: this pins down that the roundtrip
/// test above fails for save-state bugs specifically, not for general
/// nondeterminism leaking in from the host.
#[test]
fn independent_builds_agree_at_the_split_point() {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping savestate roundtrip; run with --release \
             (a debug emulator is too slow for the emulated boot)"
        );
        return;
    }
    pin_bundled_aros();

    let started = Instant::now();
    let (a, _) = boot_to_split().expect("build and boot machine A");
    let (b, _) = boot_to_split().expect("build and boot machine B");
    let fa = Fingerprint::capture(&a);
    let fb = Fingerprint::capture(&b);
    println!(
        "two builds to T={SPLIT_SECS}s in {:.1}s wall total",
        started.elapsed().as_secs_f64()
    );
    assert_eq!(
        fa, fb,
        "two machines built and stepped identically diverged before any \
         save state was involved:\n  A = {fa:?}\n  B = {fb:?}"
    );
}
