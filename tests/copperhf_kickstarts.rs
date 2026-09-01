// SPDX-License-Identifier: GPL-3.0-or-later

//! copperhf.device M6 integration matrix against real Kickstart ROMs
//! (`COPPERHF-DEVICE-PLAN.md`'s "M6 -- Tests, CI, docs": {RDB, RDB-less} x
//! {OFS, FFS-from-LSEG, PFS3-DS >4 GiB}, run against Kickstart 1.3/3.1/3.2).
//! Unlike `tests/copperhf_mounter.rs` (bundled AROS, default CI), every
//! test here is `#[ignore]` and needs a local ROM/filesystem-binary asset;
//! each checks for its own assets first and skips cleanly (passing) when
//! they are absent, per `tests/README.md`'s conventions.
//!
//! This machine (see the module comment on individual tests) has only
//! `KICK31.ROM` locally, so the Kickstart 3.1 OFS axes below actually PASS
//! here; `KICK13.ROM`, a `KICK32.ROM`, `test-assets/copperhf/
//! FastFileSystem`, and `test-assets/copperhf/pfs3aio` are all absent, so
//! their tests are only verified to skip cleanly on this machine -- their
//! logic is otherwise unexercised locally and should be treated as
//! reviewed-but-locally-unverified until someone with those assets runs
//! them.
//!
//! Image builders (OFS/FFS payload, RDSK/PART, checksum, root-directory
//! lookup) are copied from `tests/copperhf_mounter.rs`/`tests/
//! copperhf_m5.rs` rather than factored into a shared module, continuing
//! this milestone's own established no-shared-module precedent (see those
//! files' header comments) -- extended here with FSHD/LSEG block builders,
//! which no earlier milestone's tests needed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use copperline::diskimage::{FileSystem, Variant};

const SECTOR_SIZE: usize = 512;
/// 16 surfaces x 32 sectors, matching `RDB_HEADS`/`RDB_SPT` in
/// `src/harddrive.rs` and every other copperhf test's own copy of this
/// constant.
const CYL_SECTORS: u32 = 16 * 32;
const CYL_BYTES: u64 = CYL_SECTORS as u64 * SECTOR_SIZE as u64;

// --- asset lookup, copied from tests/image_regression.rs's asset_dir/
// have_required_files (private to that module, so re-implemented here) ---

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Where local ROM/filesystem-binary assets live: `COPPERLINE_TEST_ASSETS`
/// if set, else `test-assets/` under the repo root if it exists, else the
/// repo root itself. See `tests/README.md`.
fn asset_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("COPPERLINE_TEST_ASSETS") {
        return PathBuf::from(dir);
    }
    let dir = repo_root().join("test-assets");
    if dir.is_dir() {
        return dir;
    }
    repo_root()
}

fn find_asset(name: &str) -> Option<PathBuf> {
    for dir in [asset_dir(), repo_root()] {
        let path = dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn skip_if_missing(tag: &str, names: &[&str]) -> Option<Vec<PathBuf>> {
    let mut found = Vec::with_capacity(names.len());
    let mut missing = Vec::new();
    for &name in names {
        match find_asset(name) {
            Some(p) => found.push(p),
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        eprintln!("skipping {tag}; missing local assets: {missing:?} (see tests/README.md)");
        return None;
    }
    Some(found)
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines = text.lines().collect::<Vec<_>>();
    lines[lines.len().saturating_sub(line_count)..].join("\n")
}

fn toml_path(path: &Path) -> String {
    format!("'{}'", path.display())
}

fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "copperline-copperhf-kickstarts-{name}-{}",
        std::process::id()
    ))
}

// --- OFS/FFS payload -------------------------------------------------------

/// A minimal bootable volume in `filesystem` whose `S/Startup-Sequence`
/// writes `marker` into a new file `SYS:bootmark` -- the same trick
/// `tests/copperhf_mounter.rs` uses. `Echo` is a Kickstart-2.0-resident
/// internal shell command (real Kickstart 3.1/3.2, not just AROS), so this
/// needs no external `C:` commands staged onto the volume for those two ROMs.
fn build_marker_payload(volume: &str, filesystem: FileSystem, marker: &str) -> Vec<u8> {
    let src = std::env::temp_dir().join(format!(
        "copperline-copperhf-kickstarts-src-{volume}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(src.join("S")).expect("create S/");
    std::fs::write(
        src.join("S").join("Startup-Sequence"),
        format!("FailAt 21\nEcho >\"SYS:bootmark\" \"{marker}\"\n"),
    )
    .expect("write Startup-Sequence");
    let image = copperline::dirfs::build_image(&src, volume, filesystem).expect("build image");
    std::fs::remove_dir_all(&src).ok();
    image
}

/// Kickstart 1.3 has no ROM-resident `Echo`: an empty Startup-Sequence
/// leaves AmigaDOS at an open CLI prompt on the mounted volume, which is
/// verified by golden screenshot instead (see the 1.3 tests below).
fn build_empty_startup_payload(volume: &str, filesystem: FileSystem) -> Vec<u8> {
    let src = std::env::temp_dir().join(format!(
        "copperline-copperhf-kickstarts-empty-{volume}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(src.join("S")).expect("create S/");
    std::fs::write(src.join("S").join("Startup-Sequence"), "; empty\n").expect("write empty S-S");
    let image = copperline::dirfs::build_image(&src, volume, filesystem).expect("build image");
    std::fs::remove_dir_all(&src).ok();
    image
}

// --- RDSK/PART/FSHD/LSEG block builders ------------------------------------
//
// RDSK/PART mirror `tests/copperhf_mounter.rs`'s own copies (which in turn
// mirror `src/harddrive.rs`'s private `build_rdsk_block`/`build_part_block`);
// FSHD/LSEG are new for this milestone, built from `devices/hardblocks.h`'s
// field layout (via the ndk32-autodocs skill) rather than any borrowed
// source. Every block here is exactly 512 bytes; the sum-to-zero checksum
// (offset +8, longword sum of the first `summed_longs` longs including the
// zeroed checksum field, made to sum to zero) is shared by every RDB-family
// block, RDSK/PART/FSHD/LSEG alike.

fn put_be32(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn rdb_checksum(block: &mut [u8], summed_longs: usize) {
    put_be32(block, 8, 0);
    let mut sum = 0u32;
    for i in 0..summed_longs {
        sum = sum.wrapping_add(u32::from_be_bytes(
            block[i * 4..i * 4 + 4].try_into().unwrap(),
        ));
    }
    put_be32(block, 8, 0u32.wrapping_sub(sum));
}

fn build_rdsk_block(total_cyls: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"RDSK");
    put_be32(&mut b, 4, 64); // rdb_SummedLongs
    put_be32(&mut b, 12, 7); // rdb_HostID
    put_be32(&mut b, 16, SECTOR_SIZE as u32); // rdb_BlockBytes
    put_be32(&mut b, 20, 0x17); // rdb_Flags: last disk/LUN/ID
    put_be32(&mut b, 24, !0); // rdb_BadBlockList: none
    put_be32(&mut b, 28, 1); // rdb_PartitionList: sector 1
    put_be32(&mut b, 32, 2); // rdb_FileSysHeaderList: sector 2 (see build_fshd_block)
    put_be32(&mut b, 36, !0); // rdb_DriveInit
    put_be32(&mut b, 40, !0);
    for off in (44..64).step_by(4) {
        put_be32(&mut b, off, !0); // rdb_Reserved1 tail
    }
    put_be32(&mut b, 64, total_cyls); // rdb_Cylinders
    put_be32(&mut b, 68, 32); // rdb_Sectors
    put_be32(&mut b, 72, 16); // rdb_Heads
    put_be32(&mut b, 76, 1); // rdb_Interleave
    put_be32(&mut b, 80, total_cyls); // rdb_Park
    put_be32(&mut b, 96, !0); // rdb_WritePreComp
    put_be32(&mut b, 100, !0); // rdb_ReducedWrite
    put_be32(&mut b, 104, 3); // rdb_StepRate
    put_be32(&mut b, 128, 0); // rdb_RDBBlocksLo
    put_be32(&mut b, 132, CYL_SECTORS - 1); // rdb_RDBBlocksHi
    put_be32(&mut b, 136, 1); // rdb_LoCylinder
    put_be32(&mut b, 140, total_cyls - 1); // rdb_HiCylinder
    put_be32(&mut b, 144, CYL_SECTORS); // rdb_CylBlocks
    rdb_checksum(&mut b, 64);
    b
}

fn build_part_block(total_cyls: u32, dostype: u32, bootable: bool) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"PART");
    put_be32(&mut b, 4, 64); // pb_SummedLongs
    put_be32(&mut b, 12, 7); // pb_HostID
    put_be32(&mut b, 16, !0); // pb_Next: none
    put_be32(&mut b, 20, u32::from(bootable)); // pb_Flags: PBFF_BOOTABLE
    let name = b"DH0";
    b[36] = name.len() as u8;
    b[37..37 + name.len()].copy_from_slice(name);
    let env: [u32; 17] = [
        16,                       // table size
        (SECTOR_SIZE / 4) as u32, // longs per block
        0,                        // sec org
        16,                       // surfaces
        1,                        // sectors per block
        32,                       // blocks per track
        2,                        // DOS reserved blocks
        0,                        // prealloc
        0,                        // interleave
        1,                        // low cylinder
        total_cyls - 1,           // high cylinder
        30,                       // buffers
        0,                        // buffer memory type
        0x00FF_FFFF,              // max transfer
        0x7FFF_FFFE,              // mask
        6,                        // boot priority: ahead of DF0's 5
        dostype,
    ];
    for (i, v) in env.iter().enumerate() {
        put_be32(&mut b, 128 + i * 4, *v);
    }
    rdb_checksum(&mut b, 64);
    b
}

/// `FileSysHeaderBlock` at sector 2 (`rdb_FileSysHeaderList`, see
/// `build_rdsk_block`), pointing at a `LoadSegBlock` chain starting at
/// `seglist_block`. `fhb_PatchFlags = 0x180` (substitute SegList &
/// GlobalVec into the DeviceNode, per `devices/hardblocks.h`'s own comment
/// on that field) and `fhb_GlobalVec = -1` are the two fields
/// `guest/copperhf/mounter.c` actually consumes; the rest are populated for
/// completeness but not load-bearing for the mounter.
fn build_fshd_block(dostype: u32, seglist_block: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"FSHD");
    put_be32(&mut b, 4, 64); // fhb_SummedLongs (struct is exactly 64 longs)
    put_be32(&mut b, 12, 7); // fhb_HostID
    put_be32(&mut b, 16, !0); // fhb_Next: one FSHD entry is enough here
    put_be32(&mut b, 20, 0); // fhb_Flags
    put_be32(&mut b, 32, dostype); // fhb_DosType
    put_be32(&mut b, 36, 0x0001_0000); // fhb_Version: 1.0
    put_be32(&mut b, 40, 0x180); // fhb_PatchFlags
    put_be32(&mut b, 44, 0); // fhb_Type
    put_be32(&mut b, 48, 0); // fhb_Task
    put_be32(&mut b, 52, 0); // fhb_Lock
    put_be32(&mut b, 56, 0); // fhb_Handler
    put_be32(&mut b, 60, 0x2000); // fhb_StackSize
    put_be32(&mut b, 64, 0); // fhb_Priority
    put_be32(&mut b, 68, 0); // fhb_Startup
    put_be32(&mut b, 72, seglist_block); // fhb_SegListBlocks
    put_be32(&mut b, 76, 0xFFFF_FFFF); // fhb_GlobalVec: -1
    rdb_checksum(&mut b, 64);
    b
}

/// One or more `LoadSegBlock`s starting at absolute sector `start_block`,
/// carrying `data` (an ordinary LoadSeg-able hunk executable's raw bytes,
/// unmodified) 492 bytes per block, `lsb_Next` chaining forward and the
/// last block terminated with `!0`. `lsb_SummedLongs = 128`: unlike
/// RDSK/PART/FSHD (fixed-size structs, always 64 longs), an LSEG block's
/// checksummed region is the *entire* 512-byte block, since `lsb_LoadData`'s
/// content varies block to block (`devices/hardblocks.h`).
fn build_lseg_chain(start_block: u32, data: &[u8]) -> Vec<Vec<u8>> {
    const PAYLOAD: usize = 492;
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![&[]]
    } else {
        data.chunks(PAYLOAD).collect()
    };
    let mut blocks = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let mut b = vec![0u8; SECTOR_SIZE];
        b[0..4].copy_from_slice(b"LSEG");
        put_be32(&mut b, 4, 128); // lsb_SummedLongs: whole block
        put_be32(&mut b, 12, 7); // lsb_HostID
        let next = if i + 1 < chunks.len() {
            start_block + i as u32 + 1
        } else {
            0xFFFF_FFFF
        };
        put_be32(&mut b, 16, next); // lsb_Next
        b[20..20 + chunk.len()].copy_from_slice(chunk);
        rdb_checksum(&mut b, 128);
        blocks.push(b);
    }
    blocks
}

/// An image builder covering every axis this file needs: a header cylinder
/// (RDSK + PART, optionally + FSHD/LSEG carrying `fs_binary` under
/// `fs_dostype`) followed by the payload volume, which the payload's own
/// `dos_type()` supplies as the partition's dostype when `fs_dostype` is
/// `None` (the plain-OFS axes: no FSHD/LSEG needed, Kickstart already knows
/// DOS\0).
struct RdbImage {
    bytes: Vec<u8>,
    partition_start_sector: u64,
    partition_sectors: u64,
    filesystem: FileSystem,
}

fn build_rdb_image(
    payload: Vec<u8>,
    filesystem: FileSystem,
    bootable: bool,
    fs_binary_and_dostype: Option<(&[u8], u32)>,
) -> RdbImage {
    assert_eq!(payload.len() as u64 % CYL_BYTES, 0);
    let part_cyls = (payload.len() as u64 / CYL_BYTES) as u32;
    let total_cyls = 1 + part_cyls;
    let part_dostype = fs_binary_and_dostype
        .map(|(_, dostype)| dostype)
        .unwrap_or_else(|| filesystem.dos_type());

    let mut header = vec![0u8; CYL_BYTES as usize];
    header[..SECTOR_SIZE].copy_from_slice(&build_rdsk_block(total_cyls));
    header[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&build_part_block(
        total_cyls,
        part_dostype,
        bootable,
    ));
    if let Some((fs_binary, dostype)) = fs_binary_and_dostype {
        header[2 * SECTOR_SIZE..3 * SECTOR_SIZE].copy_from_slice(&build_fshd_block(dostype, 3));
        let lseg_blocks = build_lseg_chain(3, fs_binary);
        let lseg_end = 3 + lseg_blocks.len();
        assert!(
            lseg_end <= CYL_SECTORS as usize,
            "fs binary ({} bytes) does not fit in the header cylinder's spare \
             {} sectors -- grow the header area",
            fs_binary.len(),
            CYL_SECTORS as usize - 3,
        );
        for (i, block) in lseg_blocks.into_iter().enumerate() {
            let base = (3 + i) * SECTOR_SIZE;
            header[base..base + SECTOR_SIZE].copy_from_slice(&block);
        }
    } else {
        // No FSHD entry: rdb_FileSysHeaderList must read back "none".
        put_be32(&mut header[..SECTOR_SIZE], 32, !0);
        rdb_checksum(&mut header[..SECTOR_SIZE], 64);
    }

    let mut bytes = header;
    bytes.extend_from_slice(&payload);
    let partition_sectors = payload.len() as u64 / SECTOR_SIZE as u64;
    RdbImage {
        bytes,
        partition_start_sector: u64::from(CYL_SECTORS),
        partition_sectors,
        filesystem,
    }
}

/// A bare partition hardfile (no RDSK at all): the host's own
/// `HardDriveImage::open` wraps it in a synthesized RDB, exactly like
/// `tests/copperhf_mounter.rs::rdbless_image`. Only meaningful for the
/// plain-OFS axes -- FFS-from-LSEG and PFS3 both need a real FSHD/LSEG
/// chain, which only an explicit RDB image carries.
struct RdblessImage {
    bytes: Vec<u8>,
    partition_start_sector: u64,
    partition_sectors: u64,
    filesystem: FileSystem,
}

fn build_rdbless_image(payload: Vec<u8>, filesystem: FileSystem) -> RdblessImage {
    assert_eq!(payload.len() as u64 % CYL_BYTES, 0);
    assert_eq!(&payload[..3], b"DOS", "bare partition boot block magic");
    let partition_sectors = payload.len() as u64 / SECTOR_SIZE as u64;
    RdblessImage {
        bytes: payload,
        partition_start_sector: 0,
        partition_sectors,
        filesystem,
    }
}

// --- OFS/FFS root-directory lookup, generalized from tests/
// copperhf_mounter.rs to also read FFS's headerless data blocks (see
// src/dirfs.rs's own `Reader::read_file` for the same FFS/OFS split) ------

const HT_SIZE: usize = 72;

fn get32(image: &[u8], block: u64, offset: usize) -> u32 {
    let base = block as usize * SECTOR_SIZE + offset;
    u32::from_be_bytes(image[base..base + 4].try_into().unwrap())
}

fn name_of(image: &[u8], block: u64) -> Vec<u8> {
    let base = block as usize * SECTOR_SIZE + (SECTOR_SIZE - 80);
    let len = image[base] as usize;
    image[base + 1..base + 1 + len].to_vec()
}

fn dos_hash(name: &[u8]) -> usize {
    let mut h = name.len() as u32;
    for &c in name {
        h = h
            .wrapping_mul(13)
            .wrapping_add(u32::from(c.to_ascii_uppercase()))
            & 0x7FF;
    }
    (h % HT_SIZE as u32) as usize
}

fn root_block(partition_sectors: u64) -> u64 {
    (2 + partition_sectors - 1) / 2
}

fn lookup(image: &[u8], dir: u64, name: &[u8]) -> Option<u64> {
    let mut key = u64::from(get32(image, dir, 24 + dos_hash(name) * 4));
    while key != 0 {
        if name_of(image, key).eq_ignore_ascii_case(name) {
            return Some(key);
        }
        key = u64::from(get32(image, key, SECTOR_SIZE - 16));
    }
    None
}

/// Read a short (single-data-block) file back through its header's data
/// pointer, FFS or OFS.
fn read_short_file(image: &[u8], header: u64, ffs: bool) -> Vec<u8> {
    let size = get32(image, header, SECTOR_SIZE - 188) as usize;
    if size == 0 {
        return Vec::new();
    }
    let data = u64::from(get32(image, header, 24 + (HT_SIZE - 1) * 4));
    if ffs {
        let base = data as usize * SECTOR_SIZE;
        image[base..base + size.min(SECTOR_SIZE)].to_vec()
    } else {
        let data_size = get32(image, data, 12) as usize;
        let base = data as usize * SECTOR_SIZE + 24;
        image[base..base + data_size].to_vec()
    }
}

fn find_marker(
    bytes: &[u8],
    partition_start_sector: u64,
    partition_sectors: u64,
    ffs: bool,
    marker: &str,
) -> bool {
    let start = (partition_start_sector as usize) * SECTOR_SIZE;
    let partition = &bytes[start..];
    let root = root_block(partition_sectors);
    let Some(header) = lookup(partition, root, b"bootmark") else {
        return false;
    };
    let content = read_short_file(partition, header, ffs);
    String::from_utf8_lossy(&content).trim_end() == marker
}

// --- process harness ---------------------------------------------------

fn run_boot(
    tag: &str,
    machine_toml: &str,
    rom_line: &str,
    image_path: &Path,
    screenshot_at_secs: u64,
    screenshot_path: &Path,
) -> std::process::Output {
    let root = repo_root();
    let scratch = image_path.parent().unwrap();
    let config_home = scratch.join(format!("{tag}-config-home"));
    std::fs::create_dir_all(&config_home).unwrap();

    let config = scratch.join(format!("{tag}.toml"));
    std::fs::write(
        &config,
        format!(
            "{rom_line}\n\n{machine_toml}\n\n[copperhf]\nunit0 = {}\n",
            toml_path(image_path)
        ),
    )
    .unwrap();

    Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(asset_dir())
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg(screenshot_at_secs.to_string())
        .arg(screenshot_path)
        .output()
        .unwrap()
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
            "skipping copperhf.device Kickstart test ({test_name}); run with --release \
             (a debug emulator is far too slow for a full autoboot)"
        );
        return true;
    }
    false
}

// --- machine configs ---------------------------------------------------
//
// Kickstart 1.3 (V34): A500-shaped OCS/68000 config, matching `tests/
// image_regression.rs::reset_dsksync_boot_regression_reaches_boot_display`'s
// own KICK13 config exactly (no [machine] section -- the emulator's
// defaults are already this shape).
const KICK13_MACHINE: &str = r#"
[cpu]
model = "68000"

[memory]
chip = "512K"
fast = "0"

[chipset]
revision = "OCS"
"#;

// Kickstart 3.1/3.2: A1200 AGA, matching `tests/image_regression.rs`'s own
// DBLPAL_CONFIG shape for a KICK31 boot. Uniform across both ROMs so the
// only variable between the two matrix rows is the ROM file itself.
const KICK3X_MACHINE: &str = r#"
[machine]
model = "A1200"

[memory]
chip = "2M"
fast = "0"

[chipset]
revision = "AGA"
"#;

const OFS_MARKER: &str = "COPPERHF-KS-OFS-BOOTED";

// --- OFS matrix: {RDB, RDB-less} x {1.3, 3.1, 3.2} ------------------------
//
// 1.3 has no ROM-resident `Echo`, so it is verified by golden screenshot of
// the resulting CLI prompt (see `build_empty_startup_payload` above) rather
// than the bootmark file the 3.1/3.2 tests use; 3.1/3.2 both have `Echo`
// built into the ROM shell from Kickstart 2.0 onward, so they reuse exactly
// `tests/copperhf_mounter.rs`'s bootmark trick.

fn golden_path(name: &str) -> PathBuf {
    repo_root()
        .join("tests/golden/copperhf")
        .join(format!("{name}.png"))
}

/// Exact-byte golden comparison: Copperline's core is deterministic and
/// wall-clock-independent (`AGENTS.md`), so a fixed-time screenshot of an
/// unmodified boot scenario is byte-for-byte reproducible across runs --
/// the same property `tests/copperhf_m5.rs::determinism_across_repeated_
/// boots` pins for copperhf I/O specifically. No pixel-fuzz comparison is
/// needed; an exact PNG-byte match is both simpler and stricter.
fn assert_golden(tag: &str, name: &str, screenshot_path: &Path) {
    let golden = golden_path(name);
    if std::env::var_os("COPPERLINE_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::copy(screenshot_path, &golden).unwrap();
        eprintln!("[{tag}] blessed {}", golden.display());
        return;
    }
    assert!(
        golden.exists(),
        "[{tag}] missing golden {} -- generate it (with the matching local ROM \
         present) via COPPERLINE_BLESS_GOLDEN=1 cargo test --release --test \
         copperhf_kickstarts -- --ignored {tag}",
        golden.display()
    );
    let want = std::fs::read(&golden).unwrap();
    let got = std::fs::read(screenshot_path).unwrap();
    assert_eq!(
        want,
        got,
        "[{tag}] screenshot does not match the committed golden {}",
        golden.display()
    );
}

fn run_ofs_rdb_bootmark_case(tag: &str, rom_asset: &str, rom_line: &str, machine: &str) {
    if skip_if_debug(tag) {
        return;
    }
    let Some(_assets) = skip_if_missing(tag, &[rom_asset]) else {
        return;
    };

    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let payload = build_marker_payload("COPPERHF", FileSystem::OFS, OFS_MARKER);
    let image = build_rdb_image(payload, FileSystem::OFS, true, None);

    let image_path = scratch.join("unit0.hdf");
    std::fs::File::create(&image_path)
        .unwrap()
        .write_all(&image.bytes)
        .unwrap();

    let screenshot = scratch.join("shot.png");
    let output = run_boot(tag, machine, rom_line, &image_path, 60, &screenshot);
    assert_ran_ok(tag, &output);

    let bytes = std::fs::read(&image_path).unwrap();
    assert!(
        find_marker(
            &bytes,
            image.partition_start_sector,
            image.partition_sectors,
            image.filesystem.ffs,
            OFS_MARKER,
        ),
        "[{tag}] bootmark absent after boot:\nstdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
    std::fs::remove_dir_all(&scratch).ok();
}

fn run_ofs_rdbless_bootmark_case(tag: &str, rom_asset: &str, rom_line: &str, machine: &str) {
    if skip_if_debug(tag) {
        return;
    }
    let Some(_assets) = skip_if_missing(tag, &[rom_asset]) else {
        return;
    };

    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let payload = build_marker_payload("COPPERHF", FileSystem::OFS, OFS_MARKER);
    let image = build_rdbless_image(payload, FileSystem::OFS);

    let image_path = scratch.join("unit0.hdf");
    std::fs::File::create(&image_path)
        .unwrap()
        .write_all(&image.bytes)
        .unwrap();

    let screenshot = scratch.join("shot.png");
    let output = run_boot(tag, machine, rom_line, &image_path, 60, &screenshot);
    assert_ran_ok(tag, &output);

    let bytes = std::fs::read(&image_path).unwrap();
    assert!(
        find_marker(
            &bytes,
            image.partition_start_sector,
            image.partition_sectors,
            image.filesystem.ffs,
            OFS_MARKER,
        ),
        "[{tag}] bootmark absent after boot:\nstdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
    std::fs::remove_dir_all(&scratch).ok();
}

fn run_ofs_golden_case(tag: &str, rom_asset: &str, rom_line: &str, rdb: bool) {
    if skip_if_debug(tag) {
        return;
    }
    let Some(_assets) = skip_if_missing(tag, &[rom_asset]) else {
        return;
    };

    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let payload = build_empty_startup_payload("COPPERHF13", FileSystem::OFS);
    let image_path = scratch.join("unit0.hdf");
    if rdb {
        let image = build_rdb_image(payload, FileSystem::OFS, true, None);
        std::fs::File::create(&image_path)
            .unwrap()
            .write_all(&image.bytes)
            .unwrap();
    } else {
        let image = build_rdbless_image(payload, FileSystem::OFS);
        std::fs::File::create(&image_path)
            .unwrap()
            .write_all(&image.bytes)
            .unwrap();
    }

    let screenshot = scratch.join("shot.png");
    // 1.3 has no built-in Startup-Sequence commands to time against, so this
    // budget matches `reset_dsksync_boot_regression_reaches_boot_display`'s
    // own 20s KICK13 boot-to-CLI budget with headroom for the extra
    // hard-disk mount step.
    let output = run_boot(tag, KICK13_MACHINE, rom_line, &image_path, 25, &screenshot);
    assert_ran_ok(tag, &output);
    assert_golden(tag, tag, &screenshot);
    std::fs::remove_dir_all(&scratch).ok();
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 1.3 ROM asset"]
fn kick13_ofs_rdb_autoboot_reaches_cli_prompt() {
    run_ofs_golden_case(
        "kick13_ofs_rdb_autoboot_reaches_cli_prompt",
        "KICK13.ROM",
        "rom = \"KICK13.ROM\"",
        true,
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 1.3 ROM asset"]
fn kick13_ofs_rdbless_autoboot_reaches_cli_prompt() {
    run_ofs_golden_case(
        "kick13_ofs_rdbless_autoboot_reaches_cli_prompt",
        "KICK13.ROM",
        "rom = \"KICK13.ROM\"",
        false,
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.1 ROM asset"]
fn kick31_ofs_rdb_autoboots_and_writes_bootmark() {
    run_ofs_rdb_bootmark_case(
        "kick31_ofs_rdb_autoboots_and_writes_bootmark",
        "KICK31.ROM",
        "rom = \"KICK31.ROM\"",
        KICK3X_MACHINE,
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.1 ROM asset"]
fn kick31_ofs_rdbless_autoboots_and_writes_bootmark() {
    run_ofs_rdbless_bootmark_case(
        "kick31_ofs_rdbless_autoboots_and_writes_bootmark",
        "KICK31.ROM",
        "rom = \"KICK31.ROM\"",
        KICK3X_MACHINE,
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.2 ROM asset"]
fn kick32_ofs_rdb_autoboots_and_writes_bootmark() {
    run_ofs_rdb_bootmark_case(
        "kick32_ofs_rdb_autoboots_and_writes_bootmark",
        "KICK32.ROM",
        "rom = \"KICK32.ROM\"",
        KICK3X_MACHINE,
    );
}

#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.2 ROM asset"]
fn kick32_ofs_rdbless_autoboots_and_writes_bootmark() {
    run_ofs_rdbless_bootmark_case(
        "kick32_ofs_rdbless_autoboots_and_writes_bootmark",
        "KICK32.ROM",
        "rom = \"KICK32.ROM\"",
        KICK3X_MACHINE,
    );
}

// --- FFS-from-LSEG: a real FastFileSystem binary loaded through the
// FSHD/LSEG chain on Kickstart 3.1 -------------------------------------
//
// Kickstart 3.1's dos.library already ships ROM-resident DOS\0 (OFS) and
// DOS\1 (FFS) handlers, so `guest/copperhf/mounter.c`'s FileSystem.resource
// lookup (see its header comment) would skip loading a real FFS binary
// tagged DOS\1 entirely -- the LSEG path would never actually run. DOS\3
// (FFS + INTL, `FileSystem { ffs: true, variant: Variant::Intl }`) is NOT
// ROM-resident on 3.1 (only 2.0's OFS/FFS pair is baked in; the
// international/dircache variants always needed a loaded filesystem, which
// is the entire reason FastFileSystem replacement binaries existed), so
// tagging both the partition and the FSHD entry DOS\3 forces the mounter to
// actually load and run `test-assets/copperhf/FastFileSystem`'s code.
//
// The DOS\3 on-disk block layout is identical to plain FFS (same raw,
// headerless data blocks; only the international name-hashing differs, and
// this test's own filenames are plain ASCII so that never matters) --
// `src/dirfs.rs::build_image` already supports building it directly via
// `Variant::Intl`, so no bespoke FFS emitter was needed for this test.
#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.1 ROM plus \
            test-assets/copperhf/FastFileSystem"]
fn kick31_ffs_from_lseg_autoboots_and_writes_bootmark() {
    let tag = "kick31_ffs_from_lseg_autoboots_and_writes_bootmark";
    if skip_if_debug(tag) {
        return;
    }
    let Some(assets) = skip_if_missing(tag, &["KICK31.ROM", "copperhf/FastFileSystem"]) else {
        return;
    };
    let fs_binary = std::fs::read(&assets[1]).unwrap();

    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    let ffs_intl = FileSystem {
        ffs: true,
        variant: Variant::Intl,
    };
    let dostype = ffs_intl.dos_type(); // DOS\3
    let marker = "COPPERHF-KS-FFSLSEG-BOOTED";
    let payload = build_marker_payload("COPPERHFFS", ffs_intl, marker);
    let image = build_rdb_image(payload, ffs_intl, true, Some((&fs_binary, dostype)));

    let image_path = scratch.join("unit0.hdf");
    std::fs::File::create(&image_path)
        .unwrap()
        .write_all(&image.bytes)
        .unwrap();

    let screenshot = scratch.join("shot.png");
    let output = run_boot(
        tag,
        KICK3X_MACHINE,
        "rom = \"KICK31.ROM\"",
        &image_path,
        60,
        &screenshot,
    );
    assert_ran_ok(tag, &output);

    let bytes = std::fs::read(&image_path).unwrap();
    assert!(
        find_marker(
            &bytes,
            image.partition_start_sector,
            image.partition_sectors,
            true,
            marker,
        ),
        "[{tag}] bootmark absent after boot -- the LSEG-loaded FastFileSystem never \
         mounted DH0, or the Startup-Sequence never ran:\nstdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
    std::fs::remove_dir_all(&scratch).ok();
}

// --- PFS3-DS beyond 4 GiB -------------------------------------------------
//
// Scope, deliberately reduced from a full PFS3 format-and-use gate (see
// tests/README.md's "copperhf matrix" section for the fuller rationale):
// this repository has no host-side tool to FORMAT a PFS3 volume (unlike
// FFS/OFS, `src/dirfs.rs` never implemented the PFS3 on-disk layout, and
// PFS3's own `Format` is a Workbench-disk command this project does not
// carry as a test asset), so this test cannot stage a *mounted* PFS3
// volume the way the OFS/FFS-from-LSEG cases above do. What it verifies
// instead: a partition whose extent crosses the 4 GiB boundary, carrying a
// real `pfs3aio` binary loaded through FSHD/LSEG exactly like the
// FFS-from-LSEG case, boots Kickstart 3.1 to completion (attached
// alongside a normal bootable DH0 OFS unit so the machine still reaches a
// shell) without a crash, hang, or guru -- i.e. copperhf's TD64/NSD command
// path and the >4 GiB sparse backing file survive a real pfs3.handler
// actually probing the unit at startup (PFS3 issues its own TD64 reads
// during AddDosNode-time initialization even before a `Format`). It does
// NOT verify PFS3 successfully mounts a formatted volume, does NOT assert
// on `NDOS`/`PDS:` volume state, and does NOT drive any Format step.
// `tests/README.md` documents the fuller manual smoke recipe (format via a
// real Workbench install, then rerun) as the sturdier follow-up this
// automated test does not attempt.
#[test]
#[ignore = "runs the emulator and requires a local Kickstart 3.1 ROM plus \
            test-assets/copperhf/pfs3aio"]
fn kick31_pfs3_over_4gib_lseg_attach_boots_without_crashing() {
    let tag = "kick31_pfs3_over_4gib_lseg_attach_boots_without_crashing";
    if skip_if_debug(tag) {
        return;
    }
    let Some(assets) = skip_if_missing(tag, &["KICK31.ROM", "copperhf/pfs3aio"]) else {
        return;
    };
    let fs_binary = std::fs::read(&assets[1]).unwrap();

    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    // Unit 0: an ordinary bootable OFS RDB unit, so the machine reaches a
    // shell and writes bootmark regardless of what unit 1 does -- proof the
    // >4 GiB unit's presence doesn't itself wedge the boot.
    let boot_payload = build_marker_payload("COPPERHF", FileSystem::OFS, OFS_MARKER);
    let boot_image = build_rdb_image(boot_payload, FileSystem::OFS, true, None);
    let unit0_path = scratch.join("unit0.hdf");
    std::fs::File::create(&unit0_path)
        .unwrap()
        .write_all(&boot_image.bytes)
        .unwrap();

    // Unit 1: header cylinder (RDSK/PART/FSHD/LSEG) exactly like the other
    // RDB builders, but a sparse >4 GiB partition with no filesystem
    // content written into it at all (PFS3 has never formatted it) --
    // `de_HighCyl` alone is what crosses the 4 GiB boundary; the file is
    // never fully allocated on disk (`File::set_len`, a sparse hole).
    const PDS3_DOSTYPE: u32 = 0x5044_5303; // "PDS\3"
    let bytes_per_cyl = CYL_BYTES;
    let total_bytes: u64 = 5 * (1u64 << 30); // 5 GiB: crosses 4 GiB with margin
    let part_cyls = (total_bytes / bytes_per_cyl) as u32;
    let total_cyls = 1 + part_cyls;

    let mut header = vec![0u8; CYL_BYTES as usize];
    header[..SECTOR_SIZE].copy_from_slice(&build_rdsk_block(total_cyls));
    header[SECTOR_SIZE..2 * SECTOR_SIZE].copy_from_slice(&build_part_block(
        total_cyls,
        PDS3_DOSTYPE,
        false,
    ));
    header[2 * SECTOR_SIZE..3 * SECTOR_SIZE].copy_from_slice(&build_fshd_block(PDS3_DOSTYPE, 3));
    let lseg_blocks = build_lseg_chain(3, &fs_binary);
    assert!(
        3 + lseg_blocks.len() <= CYL_SECTORS as usize,
        "pfs3aio too large for header area"
    );
    for (i, block) in lseg_blocks.into_iter().enumerate() {
        let base = (3 + i) * SECTOR_SIZE;
        header[base..base + SECTOR_SIZE].copy_from_slice(&block);
    }

    let unit1_path = scratch.join("unit1.hdf");
    {
        let mut f = std::fs::File::create(&unit1_path).unwrap();
        f.write_all(&header).unwrap();
        f.set_len(CYL_BYTES + (part_cyls as u64) * bytes_per_cyl)
            .unwrap();
    }

    let root = repo_root();
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&config_home).unwrap();
    let config = scratch.join(format!("{tag}.toml"));
    std::fs::write(
        &config,
        format!(
            "rom = \"KICK31.ROM\"\n\n{KICK3X_MACHINE}\n\n[copperhf]\nunit0 = {}\nunit1 = {}\n",
            toml_path(&unit0_path),
            toml_path(&unit1_path),
        ),
    )
    .unwrap();

    let screenshot = scratch.join("shot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(asset_dir())
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("60")
        .arg(&screenshot)
        .output()
        .unwrap();
    assert_ran_ok(tag, &output);

    let bytes = std::fs::read(&unit0_path).unwrap();
    assert!(
        find_marker(
            &bytes,
            boot_image.partition_start_sector,
            boot_image.partition_sectors,
            false,
            OFS_MARKER,
        ),
        "[{tag}] unit 0's bootmark absent -- the >4 GiB PFS3-LSEG unit 1 attachment \
         appears to have prevented the machine from completing an ordinary boot:\n\
         stdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );
    std::fs::remove_dir_all(&scratch).ok();
}
