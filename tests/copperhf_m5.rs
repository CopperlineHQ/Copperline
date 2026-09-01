// SPDX-License-Identifier: GPL-3.0-or-later

//! Verification for copperhf.device's M5 milestone (asynchronous worker-
//! thread I/O, `COPPERHF-DEVICE-PLAN.md`'s "M5 -- Asynchronous I/O"): the
//! guest-visible register protocol is unchanged (`guest/copperhf/
//! copperhf_board.h` is frozen across M5 -- see `docs/internals/
//! copperhf.md`'s device-stub section), but the *host* side now answers
//! doorbells from a worker thread instead of executing them synchronously
//! on the emulation thread. Two properties are the whole point of this
//! milestone and this file pins both down against a real AROS boot that
//! does real doorbell I/O:
//!
//! - [`determinism`]: **determinism**. Copperline's core is deterministic
//!   and byte-for-byte reproducible independent of wall-clock pacing
//!   (`AGENTS.md`); moving I/O onto a worker thread must not leak wall-clock
//!   scheduling into the emulated timeline. Completions must land at
//!   emulated times that are a pure function of emulated time, not of
//!   however fast the worker thread happened to run on this particular
//!   invocation. Two runs of the same copperhf-heavy scenario are held to a
//!   savestate captured mid-boot (the sharpest artifact available: it
//!   encodes the machine's exact cycle/timeline position, not just what
//!   ended up on screen) plus a final screenshot.
//! - [`savestate_quiesce_and_resume`]: **savestate quiesce**. Saving state
//!   while copperhf I/O is in flight must block until the worker has
//!   drained, so a resumed run is byte-identical to an uninterrupted one
//!   (`AGENTS.md`'s "Save states": "A resumed run is byte-identical to an
//!   uninterrupted one"). The save point is chosen inside the busy
//!   mounter/startup-sequence I/O window specifically to exercise the
//!   quiesce path, not some quiet moment after it.
//!
//! Scenario: the M3 mounter's hand-built RDSK/PART image (see
//! `tests/copperhf_mounter.rs`, whose image-builder and OFS root-directory
//! reader are copied here rather than shared -- no common test module
//! exists in this crate yet, and per this milestone's task split, existing
//! test files are not to be edited to add one). Booting from it drives real
//! doorbell traffic end to end: the boot-ROM mounter's polled RDSK/PART walk
//! and FSHD/LSEG load, then AROS's own OFS handler reading the
//! Startup-Sequence and writing the `bootmark` marker file -- denser
//! copperhf I/O per emulated second than the M2/M4 probes' handful of
//! synchronous round trips.
//!
//! Release-only, like every other AROS-boot integration test in this suite:
//! a debug-build emulator is far too slow for a full AROS boot to a
//! Startup-Sequence.
//!
//! These tests are regression gates for the M5 *async* engine, not for
//! this milestone's absence: against a still-synchronous host
//! implementation they hold trivially (synchronous-by-construction I/O is
//! deterministic and needs no quiesce), and only start earning their keep
//! once doorbells actually complete off-thread.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use copperline::diskimage::FileSystem;

const SECTOR_SIZE: usize = 512;
/// 16 surfaces x 32 sectors, matching `RDB_HEADS`/`RDB_SPT` in
/// `src/harddrive.rs` and `tests/copperhf_mounter.rs`'s own copy of this
/// constant.
const CYL_SECTORS: u32 = 16 * 32;
const CYL_BYTES: u64 = CYL_SECTORS as u64 * SECTOR_SIZE as u64;

const MARKER: &str = "COPPERHF-M5-BOOTED";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

// --- OFS payload + RDSK/PART image, copied from tests/copperhf_mounter.rs
// (see that file's header comment for the full rationale; this milestone's
// task split forbids editing it to extract a shared helper module, and no
// common test module exists yet) ------------------------------------------

fn build_ofs_payload() -> Vec<u8> {
    let src = std::env::temp_dir().join(format!(
        "copperline-copperhf-m5-src-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(src.join("S")).expect("create S/");
    std::fs::write(
        src.join("S").join("Startup-Sequence"),
        format!("FailAt 21\nEcho >\"SYS:bootmark\" \"{MARKER}\"\n"),
    )
    .expect("write Startup-Sequence");
    let image =
        copperline::dirfs::build_image(&src, "COPPERM5", FileSystem::OFS).expect("build OFS");
    std::fs::remove_dir_all(&src).ok();
    image
}

fn put_be32(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn rdb_checksum(block: &mut [u8]) {
    put_be32(block, 8, 0);
    let mut sum = 0u32;
    for i in 0..64 {
        sum = sum.wrapping_add(u32::from_be_bytes(
            block[i * 4..i * 4 + 4].try_into().unwrap(),
        ));
    }
    put_be32(block, 8, 0u32.wrapping_sub(sum));
}

fn build_rdsk_block(total_cyls: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"RDSK");
    put_be32(&mut b, 4, 64);
    put_be32(&mut b, 12, 7);
    put_be32(&mut b, 16, SECTOR_SIZE as u32);
    put_be32(&mut b, 20, 0x17);
    put_be32(&mut b, 24, !0);
    put_be32(&mut b, 28, 1);
    put_be32(&mut b, 32, !0);
    put_be32(&mut b, 36, !0);
    put_be32(&mut b, 40, !0);
    for off in (44..64).step_by(4) {
        put_be32(&mut b, off, !0);
    }
    put_be32(&mut b, 64, total_cyls);
    put_be32(&mut b, 68, 32);
    put_be32(&mut b, 72, 16);
    put_be32(&mut b, 76, 1);
    put_be32(&mut b, 80, total_cyls);
    put_be32(&mut b, 96, !0);
    put_be32(&mut b, 100, !0);
    put_be32(&mut b, 104, 3);
    put_be32(&mut b, 128, 0);
    put_be32(&mut b, 132, CYL_SECTORS - 1);
    put_be32(&mut b, 136, 1);
    put_be32(&mut b, 140, total_cyls - 1);
    put_be32(&mut b, 144, CYL_SECTORS);
    rdb_checksum(&mut b);
    b
}

fn build_part_block(total_cyls: u32, dostype: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"PART");
    put_be32(&mut b, 4, 64);
    put_be32(&mut b, 12, 7);
    put_be32(&mut b, 16, !0);
    put_be32(&mut b, 20, 1);
    let name = b"DH0";
    b[36] = name.len() as u8;
    b[37..37 + name.len()].copy_from_slice(name);
    let env: [u32; 17] = [
        16,
        (SECTOR_SIZE / 4) as u32,
        0,
        16,
        1,
        32,
        2,
        0,
        0,
        1,
        total_cyls - 1,
        30,
        0,
        0x00FF_FFFF,
        0x7FFF_FFFE,
        6,
        dostype,
    ];
    for (i, v) in env.iter().enumerate() {
        put_be32(&mut b, 128 + i * 4, *v);
    }
    rdb_checksum(&mut b);
    b
}

struct Image {
    bytes: Vec<u8>,
    partition_start_sector: u64,
    partition_sectors: u64,
}

fn rdb_image() -> Image {
    let payload = build_ofs_payload();
    assert_eq!(payload.len() as u64 % CYL_BYTES, 0);
    let part_cyls = (payload.len() as u64 / CYL_BYTES) as u32;
    let total_cyls = 1 + part_cyls;
    let dostype = u32::from_be_bytes(payload[..4].try_into().unwrap());

    let mut bytes = vec![0u8; CYL_BYTES as usize + payload.len()];
    bytes[..SECTOR_SIZE].copy_from_slice(&build_rdsk_block(total_cyls));
    bytes[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&build_part_block(total_cyls, dostype));
    bytes[CYL_BYTES as usize..].copy_from_slice(&payload);
    let partition_sectors = payload.len() as u64 / SECTOR_SIZE as u64;
    Image {
        bytes,
        partition_start_sector: u64::from(CYL_SECTORS),
        partition_sectors,
    }
}

// --- OFS root-directory lookup, copied from tests/copperhf_mounter.rs ----

const HT_SIZE: usize = 72;

fn ofs_get32(image: &[u8], block: u64, offset: usize) -> u32 {
    let base = block as usize * SECTOR_SIZE + offset;
    u32::from_be_bytes(image[base..base + 4].try_into().unwrap())
}

fn ofs_name(image: &[u8], block: u64) -> Vec<u8> {
    let base = block as usize * SECTOR_SIZE + (SECTOR_SIZE - 80);
    let len = image[base] as usize;
    image[base + 1..base + 1 + len].to_vec()
}

fn ofs_dos_hash(name: &[u8]) -> usize {
    let mut h = name.len() as u32;
    for &c in name {
        h = h
            .wrapping_mul(13)
            .wrapping_add(u32::from(c.to_ascii_uppercase()))
            & 0x7FF;
    }
    (h % HT_SIZE as u32) as usize
}

fn ofs_root_block(partition_sectors: u64) -> u64 {
    (2 + partition_sectors - 1) / 2
}

fn ofs_lookup(image: &[u8], dir: u64, name: &[u8]) -> Option<u64> {
    let mut key = u64::from(ofs_get32(image, dir, 24 + ofs_dos_hash(name) * 4));
    while key != 0 {
        if ofs_name(image, key).eq_ignore_ascii_case(name) {
            return Some(key);
        }
        key = u64::from(ofs_get32(image, key, SECTOR_SIZE - 16));
    }
    None
}

fn ofs_read_short_file(image: &[u8], header: u64) -> Vec<u8> {
    let size = ofs_get32(image, header, SECTOR_SIZE - 188) as usize;
    if size == 0 {
        return Vec::new();
    }
    let data = u64::from(ofs_get32(image, header, 24 + (HT_SIZE - 1) * 4));
    let data_size = ofs_get32(image, data, 12) as usize;
    let base = data as usize * SECTOR_SIZE + 24;
    image[base..base + data_size].to_vec()
}

fn find_bootmark(image: &Image) -> bool {
    let start = (image.partition_start_sector as usize) * SECTOR_SIZE;
    let partition = &image.bytes[start..];
    let root = ofs_root_block(image.partition_sectors);
    let Some(header) = ofs_lookup(partition, root, b"bootmark") else {
        return false;
    };
    let content = ofs_read_short_file(partition, header);
    String::from_utf8_lossy(&content).trim_end() == MARKER
}

// --- Harness ---------------------------------------------------------------

fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "copperline-copperhf-m5-{name}-{}",
        std::process::id()
    ))
}

/// Emulated seconds at which state is saved for the mid-boot cases: inside
/// the RDSK/PART mounter walk / OFS Startup-Sequence window
/// (`tests/copperhf_mounter.rs` budgets a full boot to this scenario at
/// 60s), well before it, so the worker still has doorbells in flight when
/// the save fires.
const SAVE_AT_SECS: u64 = 30;
/// Final screenshot budget, matching `tests/copperhf_mounter.rs`'s own
/// generous budget for this scenario.
const FINAL_AT_SECS: u64 = 65;
/// Seconds after `SAVE_AT_SECS` a resumed run is carried forward to, for the
/// quiesce/resume screenshot comparison.
const RESUME_SPAN_SECS: u64 = 20;

/// Fixed power-on RTC seed (Unix seconds), matching `tests/
/// savestate_roundtrip.rs`'s own reasoning: an unseeded guest clock reads
/// the host wall clock, which would make an OFS directory entry's stored
/// timestamp (and therefore chip RAM, and therefore any byte-for-byte
/// comparison of a savestate or a final image) differ between two
/// sequential runs of this test for a reason that has nothing to do with
/// the M5 async I/O engine. Seeding it makes the guest clock -- and so the
/// timestamps AROS's OFS handler writes into `bootmark`'s file header --
/// a pure function of emulated time instead.
const RTC_SEED: u64 = 1_111_111_109;

/// Run the RDB-mounter boot scenario against `image_path` (already written
/// to disk), saving state at `SAVE_AT_SECS` to `state_path` (if given) and
/// capturing a screenshot at `screenshot_at_secs` to `screenshot_path`.
/// Returns the process output for failure messages.
fn run_boot(
    tag: &str,
    image_path: &Path,
    config_home: &Path,
    state_path: Option<&Path>,
    screenshot_at_secs: u64,
    screenshot_path: &Path,
) -> std::process::Output {
    let root = repo_root();
    let config = config_home
        .parent()
        .expect("config_home has a parent")
        .join(format!("{tag}.toml"));
    std::fs::write(
        &config,
        format!(
            "rom = \"<bundled-aros>\"\n\n[machine]\nrtc_time = {RTC_SEED}\n\n[copperhf]\n\
             unit0 = {}\n",
            toml_path(image_path)
        ),
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_copperline"));
    cmd.current_dir(&root)
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio");
    if let Some(state_path) = state_path {
        cmd.arg("--save-state-after")
            .arg(SAVE_AT_SECS.to_string())
            .arg(state_path);
    }
    cmd.arg("--screenshot-after")
        .arg(screenshot_at_secs.to_string())
        .arg(screenshot_path);

    cmd.output().unwrap()
}

fn assert_ran_ok(tag: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "[{tag}] Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
}

fn skip_if_debug(test_name: &str) -> bool {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping copperhf.device M5 test ({test_name}); run with --release \
             (a debug emulator is far too slow for a full AROS autoboot)"
        );
        return true;
    }
    false
}

/// Byte-for-byte determinism under real copperhf I/O load: the same
/// copperhf-heavy scenario (RDB-mounter autoboot -- real doorbell traffic
/// through the mounter's polled RDSK/PART walk and the OFS bootmark write)
/// is run twice, unmodified, from the same on-disk image. A mid-boot
/// savestate is the primary artifact: unlike a screenshot, it encodes the
/// machine's exact cycle/timeline position and the full contents of chip
/// RAM (where an in-flight I/O buffer or completion queue would show up),
/// so it is sensitive to timing wobble a screenshot's rendered pixels might
/// never reveal. A nondeterministic worker-thread implementation --
/// completions ordered or timed by wall-clock scheduling luck rather than
/// purely by emulated time -- would make the two runs' savestates (and
/// quite possibly their final screenshots too) diverge under repeated runs;
/// a deterministic one holds every time.
///
/// Both runs reuse the SAME absolute image path (run sequentially from one
/// scratch directory), so any host path recorded in the state file (save
/// states store file-backed hard-drive images as paths, not sector
/// contents -- see `src/savestate.rs`'s module comment) is identical
/// between the two runs and cannot itself be a source of spurious byte
/// differences; this test does not need to parse the state format to
/// exclude wall-clock/path fields.
///
/// Limitation: a byte-identical savestate is a strong but not exhaustive
/// determinism witness -- it does not cover time-of-check/time-of-use races
/// that happen to resolve identically twice in a row. Re-running this test
/// (or running it under `--test-threads=1` repeatedly) increases confidence
/// but cannot prove determinism outright; that is why the final screenshot
/// is checked too, as an independent, coarser cross-check.
#[test]
fn determinism_across_repeated_boots() {
    if skip_if_debug("determinism_across_repeated_boots") {
        return;
    }

    let scratch = scratch_dir("determinism");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&config_home).unwrap();

    let image = rdb_image();
    let image_path = scratch.join("unit0.hdf");

    let mut state_bytes = Vec::new();
    let mut screenshot_bytes = Vec::new();
    for run in 0..2 {
        let tag = format!("determinism-run{run}");
        let state_path = scratch.join(format!("run{run}.clstate"));
        let screenshot_path = scratch.join(format!("run{run}.png"));

        // Reset the SAME absolute path to a pristine copy of the image
        // before each run: both runs must see identical starting disk
        // content (the previous run's boot wrote a `bootmark` file into
        // it) while also keeping the path itself byte-identical across
        // runs, so nothing about this reset can itself be the source of a
        // savestate difference below.
        std::fs::File::create(&image_path)
            .unwrap()
            .write_all(&image.bytes)
            .unwrap();

        let output = run_boot(
            &tag,
            &image_path,
            &config_home,
            Some(&state_path),
            FINAL_AT_SECS,
            &screenshot_path,
        );
        assert_ran_ok(&tag, &output);

        let state = std::fs::read(&state_path)
            .unwrap_or_else(|e| panic!("[{tag}] failed to read savestate {state_path:?}: {e}"));
        let screenshot = std::fs::read(&screenshot_path).unwrap_or_else(|e| {
            panic!("[{tag}] failed to read screenshot {screenshot_path:?}: {e}")
        });

        if run == 0 {
            state_bytes = state;
            screenshot_bytes = screenshot;
        } else {
            assert_eq!(
                state_bytes.len(),
                state.len(),
                "run 0 and run 1 savestates differ in length ({} vs {} bytes) -- \
                 the async I/O engine is not reaching the same machine state at \
                 T={SAVE_AT_SECS}s on repeated runs of an identical scenario",
                state_bytes.len(),
                state.len(),
            );
            assert_eq!(
                state_bytes, state,
                "run 0 and run 1 savestates diverge at T={SAVE_AT_SECS}s -- the \
                 worker-thread I/O engine is surfacing completions at times (or in \
                 an order) that are not a pure function of emulated time; both runs \
                 booted the identical RDB-mounter scenario from the same image path"
            );
            assert_eq!(
                screenshot_bytes, screenshot,
                "run 0 and run 1 final screenshots (T={FINAL_AT_SECS}s) diverge -- \
                 even though the mid-boot savestates matched (or this assertion ran \
                 first), the rendered output at the end of boot differs between \
                 identical runs"
            );
        }
    }

    // Sanity: the scenario actually exercises copperhf I/O and reaches the
    // bootmark by the final screenshot's time, on the last (run 1) image on
    // disk -- otherwise a byte-identical comparison of two runs that both
    // failed to boot would be vacuous.
    let bytes = std::fs::read(&image_path).unwrap();
    let payload_len = bytes.len() as u64 - CYL_BYTES;
    let found = Image {
        bytes,
        partition_start_sector: CYL_SECTORS.into(),
        partition_sectors: payload_len / SECTOR_SIZE as u64,
    };
    assert!(
        find_bootmark(&found),
        "sanity check failed: bootmark absent after the repeated-boot determinism \
         runs -- the scenario never actually completed the boot, making the \
         byte-identical comparison above vacuous"
    );

    std::fs::remove_dir_all(&scratch).ok();
}

/// Savestate mid-I/O resume identity: one run saves state at `SAVE_AT_SECS`
/// (inside the busy RDSK/PART-mounter / OFS-bootmark I/O window) and
/// continues to `SAVE_AT_SECS + RESUME_SPAN_SECS`, capturing a screenshot; a
/// second, independent invocation `--load-state`s that same file and
/// captures a screenshot at the same absolute emulated time. Per
/// `AGENTS.md`'s save-state contract ("A resumed run is byte-identical to
/// an uninterrupted one"), the two screenshots must match exactly -- this
/// test is that contract applied specifically to a save point where the
/// M5 worker has copperhf requests in flight, which only holds if saving
/// blocks until the worker actually quiesces (drains in-flight I/O) before
/// the state is serialized. A savestate written out from underneath live
/// I/O (no quiesce, or a quiesce that misses some in-flight request) would
/// either fail to restore that request's eventual completion at all, or
/// restore it at the wrong emulated time relative to a run that never
/// paused -- either way the two screenshots would diverge, and very likely
/// the resumed run would also fail to ever produce a bootmark.
#[test]
fn savestate_quiesce_and_resume() {
    if skip_if_debug("savestate_quiesce_and_resume") {
        return;
    }

    let scratch = scratch_dir("quiesce");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&config_home).unwrap();

    // Two independent copies of the pristine image, one per boot below --
    // both runs mutate their disk (the mounter's writes, then the
    // Startup-Sequence's bootmark), so sharing one path/file between them
    // would make the second boot start from disk content the first boot
    // already changed. Each copy still keeps the same path across the WHOLE
    // of its own run (an uninterrupted boot, or a save-then-load pair), so
    // no single boot ever sees its image path or content change mid-run.
    let image = rdb_image();
    let uninterrupted_image = scratch.join("unit0-uninterrupted.hdf");
    let split_image = scratch.join("unit0-split.hdf");
    for path in [&uninterrupted_image, &split_image] {
        std::fs::File::create(path)
            .unwrap()
            .write_all(&image.bytes)
            .unwrap();
    }

    let resume_at = SAVE_AT_SECS + RESUME_SPAN_SECS;

    // Uninterrupted run: save state at SAVE_AT_SECS (mid-I/O) purely as a
    // side effect (unused otherwise) and screenshot at resume_at, all in one
    // continuous run.
    let uninterrupted_state = scratch.join("uninterrupted.clstate");
    let uninterrupted_shot = scratch.join("uninterrupted.png");
    let out_a = run_boot(
        "quiesce-uninterrupted",
        &uninterrupted_image,
        &config_home,
        Some(&uninterrupted_state),
        resume_at,
        &uninterrupted_shot,
    );
    assert_ran_ok("quiesce-uninterrupted", &out_a);

    // Resumed run: a FRESH process saves at SAVE_AT_SECS, exits; a SECOND
    // fresh process loads that state and screenshots at RESUME_SPAN_SECS
    // later (resume_at, in the loaded machine's own absolute emulated
    // time -- see AGENTS.md: scheduled-input timestamps stay absolute
    // after --load-state).
    let split_state = scratch.join("split.clstate");
    let split_shot = scratch.join("split-unused.png");
    let out_b = run_boot(
        "quiesce-split-save",
        &split_image,
        &config_home,
        Some(&split_state),
        SAVE_AT_SECS,
        &split_shot,
    );
    assert_ran_ok("quiesce-split-save", &out_b);
    assert!(
        split_state.exists(),
        "split-save run did not produce a state file at T={SAVE_AT_SECS}s (mid-I/O \
         window) -- if the worker never quiesces this may hang or fail here instead \
         of producing a corrupt state, which is also a bug worth seeing:\nstdout:\n{}\
         \nstderr:\n{}",
        tail(&out_b.stdout, 40),
        tail(&out_b.stderr, 80),
    );

    let resumed_shot = scratch.join("resumed.png");
    let config = scratch.join("resume.toml");
    std::fs::write(
        &config,
        format!(
            "rom = \"<bundled-aros>\"\n\n[copperhf]\nunit0 = {}\n",
            toml_path(&split_image)
        ),
    )
    .unwrap();
    let out_c = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--load-state")
        .arg(&split_state)
        .arg("--screenshot-after")
        .arg(resume_at.to_string())
        .arg(&resumed_shot)
        .output()
        .unwrap();
    assert_ran_ok("quiesce-resume", &out_c);

    let uninterrupted_bytes = std::fs::read(&uninterrupted_shot).unwrap();
    let resumed_bytes = std::fs::read(&resumed_shot).unwrap();
    assert_eq!(
        uninterrupted_bytes, resumed_bytes,
        "resumed run (loaded from a state saved at T={SAVE_AT_SECS}s, mid copperhf \
         I/O) diverges from the uninterrupted run at T={resume_at}s -- the M5 worker \
         is not fully quiesced (in-flight I/O drained) before the state is written, \
         so resuming from it does not reproduce the uninterrupted run's history"
    );

    // Both the uninterrupted run and the resumed run must actually finish
    // the boot (bootmark present) -- a quiesce bug that drops or duplicates
    // an in-flight completion could plausibly still produce matching
    // (both-broken) screenshots without this check.
    for (label, path) in [
        ("uninterrupted", &uninterrupted_image),
        ("resumed", &split_image),
    ] {
        let bytes = std::fs::read(path).unwrap();
        let payload_len = bytes.len() as u64 - CYL_BYTES;
        let found = Image {
            bytes,
            partition_start_sector: CYL_SECTORS.into(),
            partition_sectors: payload_len / SECTOR_SIZE as u64,
        };
        assert!(
            find_bootmark(&found),
            "bootmark absent from the {label} run's on-disk image after the \
             quiesce/resume runs -- that run never actually completed the boot"
        );
    }

    std::fs::remove_dir_all(&scratch).ok();
}
