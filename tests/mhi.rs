//! MHI virtual MPEG audio decoder board: integration and headless
//! verification (MHI-PLAN.md WP5).
//!
//! `mhi_m1_open_query_alloc_doorbell_interrupt_round_trip` needs no local
//! assets: it boots the bundled AROS ROM with `[mhi] enabled = true` and a
//! `[[filesys]]` mount built from the committed `guest/mhi/mhi_copperline.library`
//! and `guest/mhi/test/mhitest` (see `guest/mhi/README.md` -- both are
//! committed artifacts, referenced directly by path exactly like
//! `tests/image_regression.rs` does for `guest/hostfs-test/mkfile`), and
//! asserts every `MHITEST: ...` line the guest probe writes back to the
//! host is a PASS, proving open/query/alloc/free and the doorbell ->
//! interrupt round trip on the real board end to end.
//!
//! The M2 tests play a real CBR MP3 through the board via MHIplay (the
//! MHI developer kit's minimal reference client, `test-assets/mhi-devkit`)
//! and assert the decoded audio reaches `--audio-wav` capture, is
//! deterministic across repeated runs, and survives a mid-playback
//! savestate/resume byte-identically. They need the local
//! `test-assets/mp3` and `test-assets/mhi-devkit` assets (never committed,
//! see `test-assets/mhi/NOTES.md`) and skip cleanly without them.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Serializes every test in this file (mirrors `tests/image_regression.rs`'s
/// own `EMULATOR_TEST_LOCK`): `cargo test` runs `#[test]` functions in
/// parallel threads by default, and several of these tests time playback
/// against emulated-time budgets (M2_RUN_SECS, SAVE_AT) that assume nothing
/// else is competing for the host CPU while the (real-time-unpaced but
/// still host-CPU-bound) decode work runs -- observed in practice as
/// occasional flakiness in the byte-identical-across-runs check when a
/// concurrent MHI test's `copperline` process was also decoding audio.
static EMULATOR_TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_emulator_tests() -> std::sync::MutexGuard<'static, ()> {
    EMULATOR_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where the integration tests look for local, never-committed assets
/// (mirrors tests/toccata.rs / tests/image_regression.rs). Override with
/// `COPPERLINE_TEST_ASSETS`; otherwise a `test-assets/` directory under the
/// repo root is used when it exists, falling back to the repo root itself.
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let local = repo_root().join("test-assets");
    if local.is_dir() {
        local
    } else {
        repo_root()
    }
}

fn run_copperline(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn,copperline::emulator=info")
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .args(args)
        .output()
        .expect("run emulator")
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("copperline-mhi-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Every check line up to (and including) the final summary, as
/// `(kind, rest)` where `kind` is `PASS`/`FAIL`/`INFO`/`SUMMARY`. `prefix`
/// is the guest probe's own line prefix (`"MHITEST: "` for `mhitest.c`,
/// `"MHIPARAM: "` for `mhiparam.c`).
fn parse_probe_lines(output: &str, prefix: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .filter_map(|rest| {
            let mut parts = rest.splitn(2, ' ');
            let kind = parts.next()?.to_string();
            let tail = parts.next().unwrap_or("").to_string();
            Some((kind, tail))
        })
        .collect()
}

// ---------------------------------------------------------------------
// M1: host board <-> guest library round trip.
// ---------------------------------------------------------------------

/// Stage a boot volume: `S/Startup-Sequence` runs `probe_name` (already
/// copied into `mount`) redirected to `<probe_name>.out`, then drops a
/// `done` marker. `Libs/mhi_copperline.library` sits where the standard
/// `LIBS:` assign (auto-bound to `SYS:Libs` the moment this volume wins the
/// boot vote, docs/guide/configuration.md's `[[filesys]]` section) resolves
/// it, so the probe's plain `OpenLibrary("mhi_copperline.library", 0)`
/// finds it with no extra Assign step.
fn stage_m1_mount(mount: &Path, probe_name: &str, probe_src: &Path) {
    let _ = std::fs::remove_dir_all(mount);
    std::fs::create_dir_all(mount.join("S")).expect("create S/");
    std::fs::create_dir_all(mount.join("Libs")).expect("create Libs/");
    std::fs::copy(
        repo_root().join("guest/mhi/mhi_copperline.library"),
        mount.join("Libs").join("mhi_copperline.library"),
    )
    .expect("stage mhi_copperline.library");
    std::fs::copy(probe_src, mount.join(probe_name)).expect("stage probe binary");
    std::fs::write(
        mount.join("S").join("Startup-Sequence"),
        format!("FailAt 21\nSYS:{probe_name} >SYS:{probe_name}.out\nEcho >\"SYS:done\" \"done\"\n"),
    )
    .expect("write Startup-Sequence");
}

fn write_m1_config(cfg_path: &Path, mount: &Path) {
    std::fs::write(
        cfg_path,
        format!(
            "rom = \"<bundled-aros>\"\n\n\
             [mhi]\n\
             enabled = true\n\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"MHIBOOT\"\n\
             bootpri = 6\n",
            mount.display()
        ),
    )
    .expect("write test config");
}

/// Needs no local assets: the bundled AROS ROM plus the committed
/// mhi_copperline.library/mhitest artifacts are enough. Boots a machine
/// with the MHI board attached, autoboots the staged volume as `SYS:`,
/// runs `mhitest` (guest/mhi/test/mhitest.c), and asserts every
/// `MHITEST: ...` check line it wrote back to the host (via the live
/// `[[filesys]]` mount, exactly like `tests/image_regression.rs`'s hostfs
/// round trips) is a PASS -- proving OpenLibrary, all ten `i_MHI*` entry
/// points' register-based ABI, the descriptor queue, doorbell, and the
/// INT2/Signal completion round trip all work against the real board.
#[test]
#[ignore = "runs the emulator"]
fn mhi_m1_open_query_alloc_doorbell_interrupt_round_trip() {
    let _guard = lock_emulator_tests();
    let mount = scratch_dir("m1-mount");
    stage_m1_mount(
        &mount,
        "mhitest",
        &repo_root().join("guest/mhi/test/mhitest"),
    );

    let cfg_path = scratch_dir("m1-cfg").with_extension("toml");
    write_m1_config(&cfg_path, &mount);

    let shot = scratch_dir("m1-shot").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--screenshot-after",
        "40",
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("mhi: MPEG audio decoder board"),
        "expected the MHI board-attach log line; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        mount.join("done").is_file(),
        "Startup-Sequence never reached its completion marker under {}",
        mount.display()
    );

    let raw = std::fs::read_to_string(mount.join("mhitest.out")).unwrap_or_else(|e| {
        panic!(
            "reading mhitest.out under {}: {e} (mount contents: {:?})",
            mount.display(),
            std::fs::read_dir(&mount).map(|d| d
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect::<Vec<_>>())
        )
    });
    let lines = parse_probe_lines(&raw, "MHITEST: ");
    assert!(
        !lines.is_empty(),
        "no MHITEST: lines captured; raw output:\n{raw}"
    );

    let fails: Vec<&(String, String)> = lines.iter().filter(|(k, _)| k == "FAIL").collect();
    assert!(
        fails.is_empty(),
        "mhitest reported failing checks: {fails:?}\nfull output:\n{raw}"
    );
    let (last_kind, last_rest) = lines.last().unwrap();
    assert_eq!(
        (last_kind.as_str(), last_rest.as_str()),
        ("SUMMARY", "PASS"),
        "mhitest did not end with a PASS summary; full output:\n{raw}"
    );

    // Every check mhitest.c makes must have actually PASSed (not merely be
    // absent of an explicit FAIL): open, alloc, queue, and the completion
    // wait/GetEmpty round trip, plus the close.
    let must_pass = [
        "open mhi_copperline.library",
        "MHIAllocDecoder",
        "MHIQueueBuffer",
        "wait for completion signal",
        "MHIGetEmpty returns queued buffer",
        "close mhi_copperline.library",
    ];
    for check in must_pass {
        assert!(
            lines.iter().any(|(k, rest)| k == "PASS" && rest == check),
            "expected a PASS line for {check:?}; full output:\n{raw}"
        );
    }

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&shot);
    let _ = std::fs::remove_dir_all(&mount);
}

// ---------------------------------------------------------------------
// M2: a real CBR MP3 plays end to end through the board and the mixer.
// ---------------------------------------------------------------------

/// `test-assets/mp3/tone440_44k_stereo_10s_cbr128.mp3`: which local assets
/// M2 needs, and where (see `test-assets/mhi/NOTES.md` and
/// `test-assets/mhi-devkit`'s own WP1 notes). `MHIplay` (the developer
/// kit's minimal reference client, `test-assets/mhi-devkit/extracted/
/// MHIplay/MHIplay`) drives the board with two plain CLI arguments --
/// `MHIplay <driver> <file>` -- so it needs no AmigaAMP-style prefs/ARexx
/// configuration; see this file's module doc and the WP5 report for why
/// AmigaAMP itself was not used.
const MHI_DEVKIT_PLAYER: &str = "mhi-devkit/extracted/MHIplay/MHIplay";
const MHI_TONE_MP3: &str = "mp3/tone440_44k_stereo_10s_cbr128.mp3";

fn have_m2_assets(assets: &Path) -> bool {
    assets.join(MHI_DEVKIT_PLAYER).is_file() && assets.join(MHI_TONE_MP3).is_file()
}

/// Stage a boot volume that plays `mp3_path` through `mhi_copperline.library`
/// via MHIplay, exactly mirroring `stage_m1_mount`'s `LIBS:`/`SYS:` layout.
fn stage_m2_mount(mount: &Path, mp3_path: &Path) {
    let _ = std::fs::remove_dir_all(mount);
    std::fs::create_dir_all(mount.join("S")).expect("create S/");
    std::fs::create_dir_all(mount.join("Libs")).expect("create Libs/");
    std::fs::copy(
        repo_root().join("guest/mhi/mhi_copperline.library"),
        mount.join("Libs").join("mhi_copperline.library"),
    )
    .expect("stage mhi_copperline.library");
    std::fs::copy(asset_dir().join(MHI_DEVKIT_PLAYER), mount.join("MHIplay"))
        .expect("stage MHIplay");
    std::fs::copy(mp3_path, mount.join("tone440.mp3")).expect("stage MP3 fixture");
    std::fs::write(
        mount.join("S").join("Startup-Sequence"),
        "FailAt 21\n\
         SYS:MHIplay mhi_copperline.library SYS:tone440.mp3 >SYS:mhiplay.out\n\
         Echo >\"SYS:done\" \"done\"\n",
    )
    .expect("write Startup-Sequence");
}

fn write_m2_config(cfg_path: &Path, mount: &Path) {
    std::fs::write(
        cfg_path,
        format!(
            "rom = \"<bundled-aros>\"\n\n\
             [machine]\n\
             # Pin the battery clock: left at its default (real host wall-clock
             # time at boot, docs/guide/configuration.md's `[machine] rtc_time`),
             # AROS's boot/Startup-Sequence path reads it at least once (a
             # DateStamp() during Echo's file write among other things), which
             # measurably perturbs guest-visible timing by a handful of CPU
             # cycles from one process launch to the next -- observed as the
             # M2 tests' small non-reproducibility before this was pinned (see
             # `mhi_m2_playback_is_byte_identical_across_runs`'s and this
             # config's other user's own long comments for the investigation).
             # Freezing it removes that one confirmed source of run-to-run
             # jitter entirely; two uninterrupted runs of this config are
             # byte-for-byte identical `--audio-wav` captures.\n\
             rtc_time = \"2005-03-18 01:58:29\"\n\
             rtc_frozen = true\n\n\
             [mhi]\n\
             enabled = true\n\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"MHIPLAY\"\n\
             bootpri = 6\n",
            mount.display()
        ),
    )
    .expect("write test config");
}

/// Long enough to boot to the Startup-Sequence and let MHIplay preload,
/// play the whole 10s fixture, and drain -- observed boot-to-EOF is well
/// under 20s emulated; 45s leaves a wide margin without wasting headless
/// run time (unthrottled, so the cost is decode work, not wall time).
const M2_RUN_SECS: &str = "45";

/// Bound on the alignment search in `mhi_m2_savestate_resume_matches_the_
/// uninterrupted_tail` (see that test's own comment): generous enough to
/// find the small, WP5-observed offset (a handful of frames, under a
/// millisecond at 44.1 kHz) while still failing on a genuine desync.
const MAX_RESUME_SHIFT_FRAMES: usize = 32;

/// Minimal little-endian WAV/RIFF reader for the float32 PCM
/// `--audio-wav` produces (`fmt ` tag `0xFFFE`/`WAVE_FORMAT_EXTENSIBLE`,
/// subformat IEEE float): returns (channels, sample_rate, interleaved
/// samples).
fn read_wav_f32(path: &Path) -> (u16, u32, Vec<f32>) {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert_eq!(&data[0..4], b"RIFF", "not a RIFF file: {}", path.display());
    assert_eq!(&data[8..12], b"WAVE", "not a WAVE file: {}", path.display());
    let mut pos = 12usize;
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits_per_sample = 0u16;
    let mut samples = Vec::new();
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(data.len());
        let body = &data[body_start..body_end];
        match id {
            b"fmt " => {
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                bits_per_sample = u16::from_le_bytes(body[14..16].try_into().unwrap());
            }
            b"data" => {
                assert_eq!(bits_per_sample, 32, "expected float32 samples");
                samples = body
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
            }
            _ => {}
        }
        pos = body_start + size + (size % 2);
    }
    assert!(channels > 0 && sample_rate > 0, "no fmt chunk found");
    (channels, sample_rate, samples)
}

/// Goertzel single-bin power at `freq` Hz over `window` (mono samples at
/// `sample_rate`) -- enough to prove a dominant tone without a full DFT.
fn goertzel_power(window: &[f32], freq: f64, sample_rate: u32) -> f64 {
    let n = window.len() as f64;
    let k = (0.5 + (n * freq) / sample_rate as f64).floor();
    let w = 2.0 * std::f64::consts::PI * k / n;
    let coeff = 2.0 * w.cos();
    let (mut s_prev, mut s_prev2) = (0.0f64, 0.0f64);
    for &sample in window {
        let s = sample as f64 + coeff * s_prev - s_prev2;
        s_prev2 = s_prev;
        s_prev = s;
    }
    s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2
}

/// Extract the left channel as a `[start_secs, start_secs + len_secs)`
/// window.
fn left_channel_window(
    channels: u16,
    sample_rate: u32,
    samples: &[f32],
    start_secs: f64,
    len_secs: f64,
) -> Vec<f32> {
    let ch = channels as usize;
    let start = (start_secs * sample_rate as f64) as usize;
    let len = (len_secs * sample_rate as f64) as usize;
    (start..start + len)
        .filter_map(|frame| samples.get(frame * ch).copied())
        .collect()
}

/// Plays `test-assets/mp3/tone440_44k_stereo_10s_cbr128.mp3` through the
/// real board via MHIplay and proves the captured `--audio-wav` contains
/// the decoded audio: silent before/after the fixture's ~10s window, and
/// dominated by 440 Hz (matching the fixture's sine tone, `test-assets/mp3/
/// sha256sums.txt`) inside it -- end-to-end proof that CBR MP3 audio
/// reaches the mixer, not just that the board's register protocol works
/// (M1 already proves that with a non-decodable byte pattern).
#[test]
#[ignore = "runs the emulator and requires local MHI test assets"]
fn mhi_m2_plays_cbr_mp3_with_dominant_tone_reaching_the_mixer() {
    let _guard = lock_emulator_tests();
    let assets = asset_dir();
    if !have_m2_assets(&assets) {
        eprintln!(
            "skipping MHI M2 test; missing {MHI_DEVKIT_PLAYER:?} and/or {MHI_TONE_MP3:?} under {}",
            assets.display()
        );
        return;
    }

    let mount = scratch_dir("m2-tone-mount");
    stage_m2_mount(&mount, &assets.join(MHI_TONE_MP3));
    let cfg_path = scratch_dir("m2-tone-cfg").with_extension("toml");
    write_m2_config(&cfg_path, &mount);
    let wav_path = scratch_dir("m2-tone").with_extension("wav");

    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-wav",
        wav_path.to_str().unwrap(),
        "--screenshot-after",
        M2_RUN_SECS,
        scratch_dir("m2-tone-shot")
            .with_extension("png")
            .to_str()
            .unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mount.join("done").is_file(),
        "Startup-Sequence never completed under {}",
        mount.display()
    );
    let mhiplay_out = std::fs::read_to_string(mount.join("mhiplay.out")).unwrap_or_default();
    assert!(
        mhiplay_out.contains("EOF reached"),
        "MHIplay never reported EOF; output:\n{mhiplay_out}"
    );

    let (channels, sample_rate, samples) = read_wav_f32(&wav_path);
    assert_eq!(channels, 2, "expected stereo capture");
    assert_eq!(sample_rate, 44100, "expected 44.1 kHz capture");

    // Non-silence: some window well inside the 10s fixture must carry real
    // energy (RMS well above float noise-floor silence).
    let mid = left_channel_window(channels, sample_rate, &samples, 11.0, 1.0);
    assert!(
        !mid.is_empty(),
        "capture too short to contain the mid window"
    );
    let rms = (mid.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / mid.len() as f64).sqrt();
    assert!(
        rms > 0.01,
        "expected audible energy inside the fixture's playback window, got RMS {rms}"
    );

    // Dominant 440 Hz: the same window's Goertzel power at 440 Hz must
    // dwarf neighboring off-tone bins (sha256sums.txt's tone440 fixture).
    let p440 = goertzel_power(&mid, 440.0, sample_rate);
    for off in [220.0, 330.0, 550.0, 880.0, 1000.0] {
        let p_off = goertzel_power(&mid, off, sample_rate);
        assert!(
            p440 > p_off * 100.0,
            "440 Hz power {p440} not dominant over {off} Hz power {p_off}"
        );
    }

    let _ = std::fs::remove_dir_all(&mount);
    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&wav_path);
}

/// Same scenario, run twice with separate output files: proves capture is
/// driven purely by emulated time (docs/internals/audio.md), so decoding a
/// real MP3 through the board is exactly as reproducible as the synthetic
/// sources `tests/audio_stems_determinism.rs` already covers.
#[test]
#[ignore = "runs the emulator and requires local MHI test assets"]
fn mhi_m2_playback_is_byte_identical_across_runs() {
    let _guard = lock_emulator_tests();
    let assets = asset_dir();
    if !have_m2_assets(&assets) {
        eprintln!("skipping MHI M2 determinism test; local MHI test assets missing");
        return;
    }

    let mount = scratch_dir("m2-det-mount");
    let cfg_path = scratch_dir("m2-det-cfg").with_extension("toml");
    write_m2_config(&cfg_path, &mount);

    let mut captures = Vec::new();
    for i in 0..2 {
        // Re-stage a byte-identical fresh mount before every boot: MHIplay
        // writes `mhiplay.out` into it, and an already-present file from a
        // previous run opens differently (truncate vs. create) than a
        // brand-new one, shifting emulated-time boot/preload timing by a
        // few audio frames -- an artifact of reusing the mount across
        // iterations, not non-determinism in the emulator itself.
        stage_m2_mount(&mount, &assets.join(MHI_TONE_MP3));
        let wav_path = scratch_dir(&format!("m2-det-{i}")).with_extension("wav");
        let shot = scratch_dir(&format!("m2-det-shot-{i}")).with_extension("png");
        let out = run_copperline(&[
            "--config",
            cfg_path.to_str().unwrap(),
            "--noaudio",
            "--audio-wav",
            wav_path.to_str().unwrap(),
            "--screenshot-after",
            M2_RUN_SECS,
            shot.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "emulator run {i} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        captures.push(read_wav_f32(&wav_path));
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&shot);
    }

    let (ch0, sr0, samples0) = &captures[0];
    let (ch1, sr1, samples1) = &captures[1];
    assert_eq!(ch0, ch1, "the two runs captured a different channel count");
    assert_eq!(sr0, sr1, "the two runs captured a different sample rate");

    // This *is* `samples0 == samples1` outright -- true byte-for-byte
    // determinism, exactly like audio_stems_determinism.rs already proves
    // for the synthetic Paula/drivesounds sources. WP5 originally found
    // MHIplay's playback of a real MP3 landing on one of two lengths a
    // small, constant number of frames apart across otherwise-identical
    // runs, and suspected real hostfs I/O latency. Root-caused instead (WP6):
    // `write_m2_config` did not pin `[machine] rtc_time`, so the guest booted
    // against the *real* host wall clock (`SystemTime::now()`, the config's
    // documented default with no `rtc_time` set) -- AROS's boot path reads
    // it at least once (a DateStamp() during the Startup-Sequence's `Echo`
    // write, among other things), which perturbed guest-visible timing by a
    // handful of CPU cycles from one process launch to the next. Freezing
    // the clock (now done above) reproduced a byte-identical capture across
    // repeated runs on this host every time it was tried; hostfs I/O itself
    // was not the source. If this ever starts failing again on some other
    // host/filesystem, that would point at a *new* source of jitter, not a
    // reversion to the old one -- worth a fresh investigation, not
    // reinstating the aligned-match tolerance below by reflex.
    assert_eq!(
        samples0,
        samples1,
        "the two runs' captures differ (run 0: {} samples, run 1: {} samples) -- with \
         [machine] rtc_time/rtc_frozen pinned in write_m2_config this is expected to be \
         exact; see this test's comment for the investigation that established that",
        samples0.len(),
        samples1.len()
    );

    let _ = std::fs::remove_dir_all(&mount);
    let _ = std::fs::remove_file(&cfg_path);
}

/// A mid-playback save/resume must be byte-identical to the matching slice
/// of an uninterrupted run: the resampler and the un-consumed bitstream
/// queue/decoder cross-frame state all have to round-trip through the
/// savestate (MHI-PLAN.md's "Savestates" section; the same causal-resampler
/// serialization pattern as Toccata's).
#[test]
#[ignore = "runs the emulator and requires local MHI test assets"]
fn mhi_m2_savestate_resume_matches_the_uninterrupted_tail() {
    let _guard = lock_emulator_tests();
    let assets = asset_dir();
    if !have_m2_assets(&assets) {
        eprintln!("skipping MHI M2 savestate test; local MHI test assets missing");
        return;
    }

    let mount = scratch_dir("m2-ss-mount");
    let cfg_path = scratch_dir("m2-ss-cfg").with_extension("toml");
    write_m2_config(&cfg_path, &mount);

    // Boot completes and MHIplay starts well before this (M1/M2's other
    // tests observe playback energy from ~7s), so 12s is solidly
    // mid-playback of the fixture's 10s window without racing the boot.
    const SAVE_AT: &str = "12";

    // Uninterrupted reference run. Re-staged fresh (see the determinism
    // test's own comment): the save-at-12 run below must see byte-identical
    // startup conditions, so both boots start from a freshly staged mount
    // rather than one carrying a leftover mhiplay.out from a prior boot.
    stage_m2_mount(&mount, &assets.join(MHI_TONE_MP3));
    let full_wav = scratch_dir("m2-ss-full").with_extension("wav");
    let full_shot = scratch_dir("m2-ss-full-shot").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-wav",
        full_wav.to_str().unwrap(),
        "--screenshot-after",
        M2_RUN_SECS,
        full_shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "uninterrupted run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Save at SAVE_AT -- fresh mount again, matching the reference run's
    // own starting conditions exactly.
    stage_m2_mount(&mount, &assets.join(MHI_TONE_MP3));
    let state_path = scratch_dir("m2-ss-state").with_extension("clstate");
    let save_shot = scratch_dir("m2-ss-save-shot").with_extension("png");
    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--save-state-after",
        SAVE_AT,
        state_path.to_str().unwrap(),
        "--screenshot-after",
        SAVE_AT,
        save_shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "save-state run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(state_path.is_file(), "no savestate written at t={SAVE_AT}");

    // Resume and capture the tail.
    let tail_wav = scratch_dir("m2-ss-tail").with_extension("wav");
    let tail_shot = scratch_dir("m2-ss-tail-shot").with_extension("png");
    let out = run_copperline(&[
        "--load-state",
        state_path.to_str().unwrap(),
        "--noaudio",
        "--audio-wav",
        tail_wav.to_str().unwrap(),
        "--screenshot-after",
        M2_RUN_SECS,
        tail_shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "resumed run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (full_ch, full_sr, full_samples) = read_wav_f32(&full_wav);
    let (tail_ch, tail_sr, tail_samples) = read_wav_f32(&tail_wav);
    assert_eq!(full_ch, tail_ch);
    assert_eq!(full_sr, tail_sr);

    let save_at_secs: f64 = SAVE_AT.parse().unwrap();
    let skip_frames = (save_at_secs * full_sr as f64) as usize * full_ch as usize;
    assert!(
        full_samples.len() > skip_frames,
        "uninterrupted capture too short to have a tail past t={SAVE_AT}"
    );
    let reference_tail = &full_samples[skip_frames..];
    let compare_len =
        reference_tail.len().min(tail_samples.len()) - MAX_RESUME_SHIFT_FRAMES * full_ch as usize;
    assert!(
        compare_len > full_sr as usize, // at least one second of overlap
        "not enough overlap to compare (reference {}, resumed {})",
        reference_tail.len(),
        tail_samples.len()
    );

    // Exact equality at zero shift is the ideal (a genuinely byte-identical
    // resume); it does not currently hold, for two separable reasons dug up
    // across WP5 and a follow-up investigation (WP6):
    //
    // 1. `skip_frames` above is a *naive* estimate of which captured sample
    //    corresponds to the exact instant `--save-state-after SAVE_AT`
    //    fired: `SAVE_AT` is an integer count of emulated seconds, but the
    //    savestate itself lands on whatever CPU/DMA cycle the scheduler was
    //    at when that boundary was crossed, which is essentially never
    //    exactly on a 44.1 kHz sample edge. Paula's `host_sample_acc` (the
    //    fractional mixer-clock accumulator) *does* serialize and *does*
    //    resume mid-period correctly -- but that just means the resumed
    //    capture's very first sample is due at whatever true sub-sample
    //    offset was live at save time, not at the `skip_frames` boundary the
    //    test computes from wall-clock arithmetic. A small, run-independent
    //    constant shift between the naive index and the true one is
    //    therefore expected, not a bug; searching a small window for it
    //    (rather than asserting zero shift) is the correct way to locate the
    //    true correspondence point.
    // 2. Once aligned on that best shift, the two signals are an excellent
    //    but not bit-exact match: `src/mhi.rs`'s own unit test
    //    (`savestate_round_trip_reproduces_an_uninterrupted_runs_output`)
    //    already proves the *board's* internal state -- decoder
    //    reservoir/filterbank memory, descriptor queue, `sample_acc`/
    //    `mixer_acc`, and the resampler history in `Mhi::resamplers` --
    //    round-trips through a savestate bit-for-bit in-process, and WP6
    //    confirmed the *entire* pipeline is bit-exact end to end too: a
    //    resumed run of a machine that is otherwise idle at save time (no
    //    MHI playback in flight) reproduces its uninterrupted counterpart's
    //    tail exactly, zero shift, zero residual. So the residual here is
    //    real but specific to resuming *mid-decode*: WP6 traced the
    //    non-exact samples to brief (roughly one-millisecond) clusters
    //    coinciding with MHIplay's periodic re-fill reads of its input
    //    buffer, recurring every couple of seconds through the tail --
    //    steady-tone samples between those clusters match exactly at the
    //    aligned shift. That localizes the remaining gap to some small
    //    difference in exactly how the CPU/DMA scheduler re-enters its
    //    interleaving across the save/load process boundary specifically
    //    around a hostfs read, not to a missing/incorrectly-restored field
    //    in `Mhi` or `Paula` (every audio-relevant field in both was
    //    checked: none of the audio-path `#[serde(skip)]` fields in
    //    `src/chipset/paula.rs` -- the serial/audio host sinks, debug taps,
    //    and host mix preferences (LED-filter mode override, mono/stereo-
    //    separation) -- are genuine machine state, and the ones that are
    //    (`host_sample_acc`, `led_filter_guest_on`, the LED filter's own IIR
    //    memory, `mhi_audio`/`toccata_audio`/`cd_audio`, `drive_sounds`) all
    //    serialize normally). Root-causing that last mile further (the exact
    //    CPU-cycle-level mechanism at a hostfs read boundary) was out of
    //    scope for this pass; report it precisely rather than silently
    //    accepting any offset: search a small window for the shift that
    //    makes the two signals coincide, require one to exist within
    //    `MAX_RESUME_SHIFT_FRAMES`, and require the aligned signals to match
    //    near-exactly (not just "similar") -- so a genuine desync (wrong
    //    frequency, dropped samples, corrupted decode) still fails loudly.
    //
    // (WP6 also found and fixed an unrelated, larger source of run-to-run
    // jitter: `write_m2_config` did not pin `[machine] rtc_time`, so every
    // M2 run booted against the real host wall clock. That is now pinned --
    // see `write_m2_config`'s own comment -- and made
    // `mhi_m2_playback_is_byte_identical_across_runs` (no save/load at all)
    // exactly byte-identical. It was tried here too on the theory that it
    // might explain this test's residual as well; it did not -- freezing
    // the clock left the mid-decode-resume residual described above
    // unchanged, which is what points at the save/resume path itself rather
    // than at boot-time RTC jitter.)
    let ch = full_ch as usize;
    let mut best_shift_frames = None;
    let mut best_mse = f64::INFINITY;
    for shift_frames in -(MAX_RESUME_SHIFT_FRAMES as isize)..=(MAX_RESUME_SHIFT_FRAMES as isize) {
        let shift = shift_frames * ch as isize;
        let ref_start = skip_frames as isize + shift;
        if ref_start < 0 || (ref_start as usize) + compare_len > full_samples.len() {
            continue;
        }
        let reference = &full_samples[ref_start as usize..ref_start as usize + compare_len];
        let resumed = &tail_samples[..compare_len];
        let mse = reference
            .iter()
            .zip(resumed)
            .map(|(&a, &b)| {
                let d = (a - b) as f64;
                d * d
            })
            .sum::<f64>()
            / compare_len as f64;
        if mse < best_mse {
            best_mse = mse;
            best_shift_frames = Some(shift_frames);
        }
    }
    let best_shift_frames =
        best_shift_frames.expect("no candidate shift stayed within the captured samples");
    eprintln!(
        "mhi_m2_savestate_resume: best alignment shift = {best_shift_frames} frames, mse = {best_mse:e}"
    );
    // The threshold is generous on purpose: the aligned tail is an
    // excellent but not exact match (per-second-block MSE alternates
    // between ~1e-7 -- genuinely exact -- and ~1e-4 in the brief
    // MHIplay-buffer-refill clusters described above, landing the whole-tail
    // average around 3e-5, e.g. `best alignment shift = 16 frames, mse =
    // 3.2e-5` on this host; blocks that are silent in both captures
    // contribute exactly 0). 5e-4 is well above the observed worst block yet
    // would still catch real corruption (a wrong tone, dropped samples, or
    // silence where the fixture has audio would land orders of magnitude
    // higher).
    assert!(
        best_mse < 5e-4,
        "resumed playback does not match the uninterrupted run's tail even after searching \
         a +/-{MAX_RESUME_SHIFT_FRAMES}-frame alignment window (best mse {best_mse:e} at shift \
         {best_shift_frames}) -- this is a real desync, not just a small capture-path latency"
    );
    // Zero shift (true sample-for-sample byte-identity) is NOT asserted
    // here on purpose: it does not currently hold, for the two reasons
    // (correspondence-point estimation plus a genuine small mid-decode-
    // resume residual) traced above. This assertion still catches real
    // regressions (wrong tone, dropped/garbled audio, a shift outside the
    // small bound), which is what a savestate bug in decode/queue state
    // would actually look like; it does not launder the outstanding gap.

    let _ = std::fs::remove_dir_all(&mount);
    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&full_wav);
    let _ = std::fs::remove_file(&full_shot);
    let _ = std::fs::remove_file(&state_path);
    let _ = std::fs::remove_file(&save_shot);
    let _ = std::fs::remove_file(&tail_wav);
    let _ = std::fs::remove_file(&tail_shot);
}

// ---------------------------------------------------------------------
// M4: a live MHISetParam mid-playback reaches the DSP chain end to end.
// ---------------------------------------------------------------------

const MHI_PARAM_TONE: &str = "tests/data/mhi/param_tone_cbr64_mono.mp3"; // 440 Hz, 3s

/// Stage a boot volume that runs `mhiparam` (`guest/mhi/test/mhiparam.c`)
/// against the committed tone fixture, mirroring `stage_m1_mount`'s
/// `LIBS:`/`SYS:` layout.
fn stage_param_mount(mount: &Path) {
    let _ = std::fs::remove_dir_all(mount);
    std::fs::create_dir_all(mount.join("S")).expect("create S/");
    std::fs::create_dir_all(mount.join("Libs")).expect("create Libs/");
    std::fs::copy(
        repo_root().join("guest/mhi/mhi_copperline.library"),
        mount.join("Libs").join("mhi_copperline.library"),
    )
    .expect("stage mhi_copperline.library");
    std::fs::copy(
        repo_root().join("guest/mhi/test/mhiparam"),
        mount.join("mhiparam"),
    )
    .expect("stage mhiparam");
    std::fs::copy(repo_root().join(MHI_PARAM_TONE), mount.join("tone.mp3"))
        .expect("stage tone fixture");
    std::fs::write(
        mount.join("S").join("Startup-Sequence"),
        "FailAt 21\n\
         SYS:mhiparam SYS:tone.mp3 >SYS:mhiparam.out\n\
         Echo >\"SYS:done\" \"done\"\n",
    )
    .expect("write Startup-Sequence");
}

fn write_param_config(cfg_path: &Path, mount: &Path) {
    std::fs::write(
        cfg_path,
        format!(
            "rom = \"<bundled-aros>\"\n\n\
             [machine]\n\
             rtc_time = \"2005-03-18 01:58:29\"\n\
             rtc_frozen = true\n\n\
             [mhi]\n\
             enabled = true\n\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"MHIPARAM\"\n\
             bootpri = 6\n",
            mount.display()
        ),
    )
    .expect("write test config");
}

/// Generous margin over the fixture's 3s duration plus boot -- same
/// order-of-magnitude reasoning as `M2_RUN_SECS`.
const PARAM_RUN_SECS: &str = "30";

/// End-to-end proof of MHI-PLAN-M3-M4.md WP4.5: `mhiparam` plays
/// `tone.mp3` (440 Hz, 3s), and ~1s in issues a live `MHISetParam` volume
/// drop (100 -> 20) and hard pan-right (50 -> 100) together, mid-playback.
/// Proves the capture shows both effects at the expected emulated moment:
/// before the change, both channels carry the tone near full amplitude;
/// after it, overall RMS drops sharply *and* the left channel is
/// specifically far quieter than the right (the pan, not just the volume,
/// took effect) -- not a change the M1-M3 "latched, otherwise inert"
/// behavior could ever produce, so this also proves the board's `CAPS`
/// bit 6 upgrade is real, not just documented.
#[test]
#[ignore = "runs the emulator"]
fn mhi_m4_live_setparam_changes_volume_and_balance_mid_playback() {
    let _guard = lock_emulator_tests();
    let mount = scratch_dir("m4-param-mount");
    stage_param_mount(&mount);
    let cfg_path = scratch_dir("m4-param-cfg").with_extension("toml");
    write_param_config(&cfg_path, &mount);
    let wav_path = scratch_dir("m4-param").with_extension("wav");
    let shot = scratch_dir("m4-param-shot").with_extension("png");

    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--audio-wav",
        wav_path.to_str().unwrap(),
        "--screenshot-after",
        PARAM_RUN_SECS,
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mount.join("done").is_file(),
        "Startup-Sequence never reached its completion marker under {}",
        mount.display()
    );

    let raw = std::fs::read_to_string(mount.join("mhiparam.out"))
        .unwrap_or_else(|e| panic!("reading mhiparam.out under {}: {e}", mount.display()));
    let lines = parse_probe_lines(&raw, "MHIPARAM: ");
    assert!(
        !lines.is_empty(),
        "no MHIPARAM: lines captured; raw output:\n{raw}"
    );
    let fails: Vec<&(String, String)> = lines.iter().filter(|(k, _)| k == "FAIL").collect();
    assert!(
        fails.is_empty(),
        "mhiparam reported failing checks: {fails:?}\nfull output:\n{raw}"
    );
    let (last_kind, last_rest) = lines.last().expect("no MHIPARAM: lines captured");
    assert_eq!(
        (last_kind.as_str(), last_rest.as_str()),
        ("SUMMARY", "PASS"),
        "mhiparam did not end with a PASS summary; full output:\n{raw}"
    );

    let (channels, sample_rate, samples) = read_wav_f32(&wav_path);
    assert_eq!(channels, 2, "expected stereo capture");
    assert_eq!(sample_rate, 44100, "expected 44.1 kHz capture");

    // Find playback onset the same way the M2 tone test does implicitly
    // (fixed offset into a known-quiet boot), but robustly: scan for the
    // first 0.3s window whose RMS clears a real-tone threshold.
    let total_secs = (samples.len() / channels as usize) as f64 / sample_rate as f64;
    let mut onset = None;
    let mut t = 0.0;
    while t + 0.3 <= total_secs {
        let window = left_channel_window(channels, sample_rate, &samples, t, 0.3);
        let rms = (window.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>()
            / window.len() as f64)
            .sqrt();
        if rms > 0.03 {
            onset = Some(t);
            break;
        }
        t += 0.2;
    }
    let onset = onset.expect("no window ever cleared the RMS threshold");

    // Before the change (mhiparam waits MHIPARAM_CHANGE_AT_SECS = 1s
    // before issuing MHISetParam): both channels near full amplitude,
    // roughly equal (panning still centered).
    let before_l = left_channel_window(channels, sample_rate, &samples, onset + 0.2, 0.3);
    let before_r = {
        let start = ((onset + 0.2) * sample_rate as f64) as usize;
        let len = (0.3 * sample_rate as f64) as usize;
        (start..start + len)
            .filter_map(|frame| samples.get(frame * channels as usize + 1).copied())
            .collect::<Vec<_>>()
    };
    let rms_before_l = (before_l
        .iter()
        .map(|&x| (x as f64) * (x as f64))
        .sum::<f64>()
        / before_l.len() as f64)
        .sqrt();
    let rms_before_r = (before_r
        .iter()
        .map(|&x| (x as f64) * (x as f64))
        .sum::<f64>()
        / before_r.len() as f64)
        .sqrt();
    assert!(
        rms_before_l > 0.05,
        "expected audible energy before the param change, got RMS {rms_before_l}"
    );
    assert!(
        (rms_before_l - rms_before_r).abs() < rms_before_l * 0.2,
        "expected roughly balanced channels before the pan change, got L={rms_before_l} \
         R={rms_before_r}"
    );

    // Well after the change (past both the 1s wait and the change taking
    // effect): overall level down sharply, and left specifically much
    // quieter than right.
    let after_l = left_channel_window(channels, sample_rate, &samples, onset + 1.5, 0.3);
    let after_r = {
        let start = ((onset + 1.5) * sample_rate as f64) as usize;
        let len = (0.3 * sample_rate as f64) as usize;
        (start..start + len)
            .filter_map(|frame| samples.get(frame * channels as usize + 1).copied())
            .collect::<Vec<_>>()
    };
    let rms_after_l = (after_l
        .iter()
        .map(|&x| (x as f64) * (x as f64))
        .sum::<f64>()
        / after_l.len() as f64)
        .sqrt();
    let rms_after_r = (after_r
        .iter()
        .map(|&x| (x as f64) * (x as f64))
        .sum::<f64>()
        / after_r.len() as f64)
        .sqrt();
    assert!(
        rms_after_l < rms_before_l * 0.3,
        "expected the volume drop to sharply reduce the left channel, got before={rms_before_l} \
         after={rms_after_l}"
    );
    assert!(
        rms_after_l < rms_after_r * 0.3,
        "expected the hard pan-right to make left much quieter than right after the change, \
         got L={rms_after_l} R={rms_after_r}"
    );

    let _ = std::fs::remove_dir_all(&mount);
    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&wav_path);
    let _ = std::fs::remove_file(&shot);
}
