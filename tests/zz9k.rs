//! zz9k crypto board: guest-side integration verification
//! (docs/internals/zz9k.md).
//!
//! Needs no local assets: boots the bundled AROS ROM with `[zz9k]`
//! enabled and a `[[filesys]]` boot volume holding the committed
//! `guest/zz9kprobe/zz9kprobe` binary -- the probe built on the *real*
//! ZZ9000 SDK transport (`guest/zz9kprobe/vendor/zz9k_host.c`, the exact
//! code `zz9k.library` and the SDK tools link) -- and asserts every
//! `ZZ9K: ...` line it writes back through the host mount is a PASS. A
//! pass proves board discovery, the bootstrap registers, mailbox
//! attach/submit/poll, shared buffers, all five crypto opcodes against
//! published vectors, and the armed completion-interrupt path (a real
//! AddIntServer ISR on INT6 acknowledging the board and signalling the
//! waiting task) work against the board end to end, driven by the same
//! Amiga-side code real ZZ9000 software runs.
//!
//! The unmodified SDK *tools* (zz9k-info, zz9k-hash, ...) have their own
//! asset-gated test in `tests/zz9k_sdk_tools.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Serializes every test in this file (mirrors `tests/mhi.rs`).
static EMULATOR_TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_emulator_tests() -> std::sync::MutexGuard<'static, ()> {
    EMULATOR_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        std::env::temp_dir().join(format!("copperline-zz9k-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Stage a boot volume: `S/Startup-Sequence` runs the committed probe
/// binary redirected to `zz9kprobe.out`, then drops a `done` marker.
fn stage_mount(mount: &Path) {
    let _ = std::fs::remove_dir_all(mount);
    std::fs::create_dir_all(mount.join("S")).expect("create S/");
    std::fs::copy(
        repo_root().join("guest/zz9kprobe/zz9kprobe"),
        mount.join("zz9kprobe"),
    )
    .expect("stage zz9kprobe binary");
    std::fs::write(
        mount.join("S").join("Startup-Sequence"),
        "FailAt 21\nSYS:zz9kprobe >SYS:zz9kprobe.out\nEcho >\"SYS:done\" \"done\"\n",
    )
    .expect("write Startup-Sequence");
}

/// The battery clock is pinned so guest-visible boot timing is identical
/// across runs (AGENTS.md's rtc_time note; same as tests/mhi.rs's
/// determinism runs).
fn write_config(cfg_path: &Path, mount: &Path) {
    std::fs::write(
        cfg_path,
        format!(
            "rom = \"<bundled-aros>\"\n\n\
             [machine]\n\
             rtc_time = \"2005-03-18 01:58:29\"\n\
             rtc_frozen = true\n\n\
             [zz9k]\n\
             enabled = true\n\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"ZZBOOT\"\n\
             bootpri = 6\n",
            mount.display()
        ),
    )
    .expect("write test config");
}

/// Boot the staged volume, wait out the probe, and return the raw
/// `zz9kprobe.out` contents.
fn run_probe(tag: &str) -> (PathBuf, String) {
    let mount = scratch_dir(&format!("{tag}-mount"));
    stage_mount(&mount);
    let cfg_path = scratch_dir(&format!("{tag}-cfg")).with_extension("toml");
    write_config(&cfg_path, &mount);
    let shot = scratch_dir(&format!("{tag}-shot")).with_extension("png");

    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--screenshot-after",
        "60",
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ZZ9000 SDK crypto board"),
        "expected the zz9k board-attach log line; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        mount.join("done").is_file(),
        "Startup-Sequence never reached its completion marker under {}",
        mount.display()
    );
    let raw = std::fs::read_to_string(mount.join("zz9kprobe.out"))
        .unwrap_or_else(|e| panic!("reading zz9kprobe.out under {}: {e}", mount.display()));

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&shot);
    (mount, raw)
}

/// Every check the probe makes must actually PASS (not merely be absent
/// of a FAIL); this list mirrors guest/zz9kprobe/zz9kprobe.c's check
/// names one for one.
const MUST_PASS: &[&str] = &[
    "open",
    "shared pool alloc",
    "query caps",
    "query crypto service",
    "absent service reports NOT_FOUND",
    "ping echo",
    "shared write + mem copy",
    "mem fill",
    "stale handle rejected",
    "sha256 abc",
    "hmac-sha256 jefe",
    "poly1305 rfc8439",
    "chacha20 rfc8439",
    "chacha20 round trip",
    "aead encrypt",
    "aead decrypt",
    "aead tag mismatch rejected",
    "aes-128-gcm round trip",
    "x25519 rfc7748",
    "p256 keygen",
    "p256 derive rfc5903",
    "ecdsa p256 verify",
    "ecdsa invalid sig reports valid=0",
    "rsa-2048 verify kat",
    "arm completion irq",
    "irq-driven call completes",
    "polled call after disarm",
    "diag counters",
];

#[test]
#[ignore = "runs the emulator"]
fn zz9k_probe_full_protocol_round_trip() {
    let _guard = lock_emulator_tests();
    let (mount, raw) = run_probe("probe");

    let fails: Vec<&str> = raw
        .lines()
        .filter(|line| line.starts_with("ZZ9K: FAIL"))
        .collect();
    assert!(
        fails.is_empty(),
        "zz9kprobe reported failing checks: {fails:?}\nfull output:\n{raw}"
    );
    assert!(
        raw.lines().any(|line| line == "ZZ9K: SUMMARY PASS"),
        "zz9kprobe did not end with a PASS summary; full output:\n{raw}"
    );
    for check in MUST_PASS {
        let line = format!("ZZ9K: PASS {check}");
        assert!(
            raw.lines().any(|l| l == line),
            "expected {line:?}; full output:\n{raw}"
        );
    }

    let _ = std::fs::remove_dir_all(&mount);
}

/// The board is pure compute and the machine deterministic: two identical
/// runs produce byte-identical probe output.
#[test]
#[ignore = "runs the emulator"]
fn zz9k_probe_output_is_deterministic() {
    let _guard = lock_emulator_tests();
    let (mount_a, raw_a) = run_probe("det-a");
    let (mount_b, raw_b) = run_probe("det-b");
    assert!(
        raw_a.lines().any(|line| line == "ZZ9K: SUMMARY PASS"),
        "first run did not pass:\n{raw_a}"
    );
    assert_eq!(raw_a, raw_b, "probe output differs between identical runs");
    let _ = std::fs::remove_dir_all(&mount_a);
    let _ = std::fs::remove_dir_all(&mount_b);
}
