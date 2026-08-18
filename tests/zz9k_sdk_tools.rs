//! zz9k crypto board: the *unmodified* ZZ9000 SDK tools run against the
//! board (docs/internals/zz9k.md), on both bus generations.
//!
//! Needs local assets (skips cleanly without them): the SDK's own m68k
//! tool binaries under `test-assets/zz9k/C/` -- `zz9k-info`, `zz9k-hash`,
//! `zz9k-chacha`, `zz9k-aead`, `zz9k-irqtest` -- built from the
//! BlitterStudio/zz9000-sdk revision pinned in docs/internals/zz9k.md:
//!
//! ```sh
//! git clone https://github.com/BlitterStudio/zz9000-sdk
//! cd zz9000-sdk && git checkout <pinned commit>
//! docker run --rm -v "$PWD:/work" -w /work stefanreinauer/amiga-gcc:gcc-v16.1 sh -c '
//!   mkdir -p build/m68k
//!   CFLAGS="-noixemul -fcommon -Os -m68000 -s -Iinclude -Ihost/include"
//!   m68k-amigaos-gcc $CFLAGS -c host/src/zz9k_host.c -o build/m68k/zz9k_host.o
//!   for t in info hash irqtest chacha aead; do
//!     m68k-amigaos-gcc $CFLAGS build/m68k/zz9k_host.o tools/zz9k-$t.c -o build/zz9k-$t
//!   done'
//! mkdir -p <copperline>/test-assets/zz9k/C && cp build/zz9k-* <copperline>/test-assets/zz9k/C/
//! ```
//!
//! (`-fcommon` matters: the SDK's tentative `DOSBase` definition must
//! merge with the startup code's, and newer GCCs default to `-fno-common`
//! -- see guest/zz9kprobe/Makefile. The SDK's own build image is a GCC 6
//! era toolchain where `-fcommon` was still the default.)
//!
//! The staged machine mounts a second host volume named `ENV` so the
//! tools' `ENV:` variable probes (`zz9k_sdk_use_int2` opening
//! `ENV:ZZ9K_INT2`) resolve to a real volume and fail cleanly instead of
//! raising an "insert volume ENV:" requester on the minimal boot volume
//! (which has no `Assign`, and AROS mounts no `RAM:` this early).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

static EMULATOR_TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_emulator_tests() -> std::sync::MutexGuard<'static, ()> {
    EMULATOR_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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

const TOOLS: &[&str] = &[
    "zz9k-info",
    "zz9k-hash",
    "zz9k-chacha",
    "zz9k-aead",
    "zz9k-irqtest",
];

fn tools_dir() -> Option<PathBuf> {
    let dir = asset_dir().join("zz9k/C");
    if TOOLS.iter().all(|t| dir.join(t).is_file()) {
        Some(dir)
    } else {
        None
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
    let dir = std::env::temp_dir().join(format!(
        "copperline-zz9k-sdk-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn stage_mount(mount: &Path, tools: &Path) {
    let _ = std::fs::remove_dir_all(mount);
    std::fs::create_dir_all(mount.join("S")).expect("create S/");
    for tool in TOOLS {
        std::fs::copy(tools.join(tool), mount.join(tool))
            .unwrap_or_else(|e| panic!("stage {tool}: {e}"));
    }
    std::fs::write(
        mount.join("S").join("Startup-Sequence"),
        "FailAt 21\n\
         SYS:zz9k-info >SYS:info.out\n\
         SYS:zz9k-hash >SYS:hash.out\n\
         SYS:zz9k-hash --alg sha1 >SYS:sha1.out\n\
         SYS:zz9k-hash --alg poly1305 >SYS:poly.out\n\
         SYS:zz9k-hash --hmac Jefe \"what do ya want for nothing?\" >SYS:hmac.out\n\
         SYS:zz9k-chacha >SYS:chacha.out\n\
         SYS:zz9k-aead >SYS:aead.out\n\
         SYS:zz9k-irqtest >SYS:irqtest.out\n\
         Echo >\"SYS:done\" \"done\"\n",
    )
    .expect("write Startup-Sequence");
}

fn write_config(cfg_path: &Path, mount: &Path, env_mount: &Path, zorro: Option<&str>) {
    std::fs::create_dir_all(env_mount).expect("create ENV mount");
    let zz9k = match zorro {
        Some(extra) => format!("[zz9k]\nenabled = true\n{extra}\n"),
        None => "[zz9k]\nenabled = true\n".to_string(),
    };
    std::fs::write(
        cfg_path,
        format!(
            "rom = \"<bundled-aros>\"\n\n\
             [machine]\n\
             rtc_time = \"2005-03-18 01:58:29\"\n\
             rtc_frozen = true\n\n\
             {zz9k}\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"ZZBOOT\"\n\
             bootpri = 6\n\n\
             [[filesys]]\n\
             path = '{}'\n\
             volume = \"ENV\"\n\
             bootpri = -128\n",
            mount.display(),
            env_mount.display()
        ),
    )
    .expect("write test config");
}

fn read_out(mount: &Path, name: &str) -> String {
    std::fs::read_to_string(mount.join(name))
        .unwrap_or_else(|e| panic!("reading {name} under {}: {e}", mount.display()))
}

fn run_and_assert(tag: &str, zorro: Option<&str>, expect_product: &str) {
    let Some(tools) = tools_dir() else {
        eprintln!("skipping: SDK tool binaries not present under test-assets/zz9k/C");
        return;
    };
    let mount = scratch_dir(&format!("{tag}-mount"));
    stage_mount(&mount, &tools);
    let env_mount = scratch_dir(&format!("{tag}-env"));
    let cfg_path = scratch_dir(&format!("{tag}-cfg")).with_extension("toml");
    write_config(&cfg_path, &mount, &env_mount, zorro);
    let shot = scratch_dir(&format!("{tag}-shot")).with_extension("png");

    let out = run_copperline(&[
        "--config",
        cfg_path.to_str().unwrap(),
        "--noaudio",
        "--screenshot-after",
        "90",
        shot.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "emulator run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(expect_product),
        "expected the board on {expect_product}; stderr:\n{stderr}"
    );
    assert!(
        mount.join("done").is_file(),
        "Startup-Sequence never completed under {}",
        mount.display()
    );

    // zz9k-info: the SDK's own view of the board matches the contract.
    let info = read_out(&mount, "info.out");
    for line in [
        "SDK ABI:              2.3",
        "Capabilities:         0x00007d07",
        "Transport:            polling doorbell irq",
        "Inline payload:       48 bytes",
        "Shared buffers:       64",
        "Request ring entries: 32",
        "Mailbox address:      0x3fe43000",
    ] {
        assert!(info.contains(line), "zz9k-info missing {line:?}:\n{info}");
    }
    assert!(
        info.contains("crypto     id=0x0800"),
        "zz9k-info did not list the crypto service:\n{info}"
    );

    // The crypto tools assert their own built-in vectors and print
    // "known vector ok" only when the board's answer matches.
    for (name, out_file) in [
        ("zz9k-hash sha256", "hash.out"),
        ("zz9k-hash sha1", "sha1.out"),
        ("zz9k-hash poly1305", "poly.out"),
        ("zz9k-chacha", "chacha.out"),
        ("zz9k-aead", "aead.out"),
    ] {
        let text = read_out(&mount, out_file);
        assert!(
            text.contains("known vector ok"),
            "{name} did not report its known vector:\n{text}"
        );
    }
    // HMAC has no built-in vector in the tool; it must at least produce
    // the digest line (RFC 4231 case 2) without an error.
    let hmac = read_out(&mount, "hmac.out");
    assert!(
        hmac.contains("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"),
        "zz9k-hash --hmac did not produce the RFC 4231 digest:\n{hmac}"
    );

    // zz9k-irqtest: its own interrupt server saw the completion interrupt.
    let irqtest = read_out(&mount, "irqtest.out");
    assert!(
        irqtest.contains("irqtest ok") && irqtest.contains("irq_hits=1"),
        "zz9k-irqtest did not pass:\n{irqtest}"
    );

    let _ = std::fs::remove_file(&cfg_path);
    let _ = std::fs::remove_file(&shot);
    let _ = std::fs::remove_dir_all(&mount);
    let _ = std::fs::remove_dir_all(&env_mount);
}

/// Zorro II (the default 68000 machine): 4M window, no doorbell -- the
/// transport polls and the board's ring scan does the pickup.
#[test]
#[ignore = "runs the emulator; needs test-assets/zz9k"]
fn zz9k_sdk_tools_pass_on_zorro_ii() {
    let _guard = lock_emulator_tests();
    run_and_assert("z2", None, "ZZ9000 SDK crypto board");
}

/// Zorro III (68030): live doorbell through the register aperture.
#[test]
#[ignore = "runs the emulator; needs test-assets/zz9k"]
fn zz9k_sdk_tools_pass_on_zorro_iii() {
    let _guard = lock_emulator_tests();
    run_and_assert(
        "z3",
        Some("zorro = 3\n[cpu]\nmodel = \"68030\""),
        "ZZ9000 SDK crypto board",
    );
}
