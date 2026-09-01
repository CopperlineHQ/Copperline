// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end verification of copperhf.device's M3 boot-ROM mounter: an
//! attached unit's partition(s) should RDB-mount and autoboot AROS the same
//! way an `[ide]`/`[scsi]`/`[lide]` unit's already do (`docs/internals/
//! copperhf.md`'s M3 milestone).
//!
//! Two image shapes exercise the mounter's one real fork -- RDB-less images
//! only reach the guest after the *host* wraps them in a synthesized RDB
//! (`src/harddrive.rs::HardDriveImage::open`), so from the boot ROM's point
//! of view every attached unit always carries a real RDSK/PART chain; there
//! is no bare-partition code path in the guest at all:
//!
//! - [`aros_autoboots_from_rdbless_image`]: a bare OFS partition hardfile,
//!   wrapped in the host's own synthesized RDB (one extra 256 KiB cylinder
//!   of RDSK+PART in front, `HardDriveImage::open`'s `bare_partition`
//!   branch).
//! - [`aros_autoboots_from_rdb_image`]: an RDSK/PART chain this test builds
//!   by hand (mirroring `build_rdsk_block`/`build_part_block` in
//!   `src/harddrive.rs`, which integration tests cannot call directly --
//!   they are private to that module) wrapping the same OFS payload.
//!
//! Both variants' payload is built with `copperline::dirfs::build_image`
//! (public, and already proven against real AROS boots by every `[ide]`/
//! `[scsi]`/`[lide]` directory-mount test in this suite): a minimal OFS
//! volume whose `S/Startup-Sequence` echoes a marker string to a new file,
//! `bootmark`, on the same volume once AROS actually reaches it. Unlike
//! `tests/copperhf_device.rs`'s M2 probe, the marker can't just be grepped
//! out of the image's raw bytes afterwards: the Startup-Sequence script
//! that is supposed to *produce* `bootmark` already contains the literal
//! marker text (it's `Echo`'s own argument), so a raw search would report
//! success on a pristine, never-booted image. [`find_bootmark`] instead
//! walks the volume's OFS root directory structurally, the same way real
//! AmigaDOS would, to confirm the file actually exists and holds the
//! marker.
//!
//! Release-only, like `tests/copperhf_device.rs`: a debug-build emulator is
//! far too slow for a full AROS boot to a Startup-Sequence.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use copperline::diskimage::FileSystem;

const SECTOR_SIZE: usize = 512;
/// 16 surfaces x 32 sectors, matching `RDB_HEADS`/`RDB_SPT` in
/// `src/harddrive.rs` (the geometry both the host's synthesized RDB and
/// this test's own hand-built one use).
const CYL_SECTORS: u32 = 16 * 32;
const CYL_BYTES: u64 = CYL_SECTORS as u64 * SECTOR_SIZE as u64;

const MARKER: &str = "COPPERHF-M3-BOOTED";

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

// --- OFS payload -----------------------------------------------------------

/// Build a minimal bootable OFS volume (via the crate's own `dirfs` builder,
/// the same code path `[ide]`/`[scsi]`/`[lide]` directory mounts use) whose
/// `S/Startup-Sequence` writes [`MARKER`] into a new file `SYS:bootmark`.
///
/// `Echo` is a Kickstart-2.0-resident internal shell command (see
/// `src/runprog.rs`), so this needs no external `C:` commands staged onto
/// the volume at all -- the marker file is written entirely by AROS's own
/// boot shell once it reaches the Startup-Sequence.
fn build_ofs_payload() -> Vec<u8> {
    let src = std::env::temp_dir().join(format!(
        "copperline-copperhf-mounter-src-{}-{}",
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
        copperline::dirfs::build_image(&src, "COPPERHF", FileSystem::OFS).expect("build OFS");
    std::fs::remove_dir_all(&src).ok();
    image
}

// --- Hand-built RDSK/PART, mirroring src/harddrive.rs ----------------------
//
// `build_rdsk_block`/`build_part_block` there are private to the crate, so
// this reimplements the same field layout for a one-partition RDB: RDSK at
// LBA 0, PART at LBA 1, both padded out to a full 256 KiB cylinder, exactly
// as `HardDriveImage::open` synthesizes one around a bare partition image --
// just written by this test instead of at open time, so the file *already*
// carries an RDSK and takes the RDB-image code path rather than the
// bare-partition one.

fn put_be32(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// Sum-to-zero longword checksum shared by RDSK and PART blocks: the
/// checksum field (offset 8) is zeroed, the first 64 big-endian longs are
/// summed with wrapping add, and the checksum is the value that makes that
/// sum zero.
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

/// Identity strings (vendor/product/revision) are left blank -- the mounter
/// has no reason to care, and no test asserts on them.
fn build_rdsk_block(total_cyls: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"RDSK");
    put_be32(&mut b, 4, 64); // size in longs
    put_be32(&mut b, 12, 7); // host id
    put_be32(&mut b, 16, SECTOR_SIZE as u32);
    put_be32(&mut b, 20, 0x17); // flags: last disk/LUN/ID
    put_be32(&mut b, 24, !0); // bad-block list: none
    put_be32(&mut b, 28, 1); // partition list at sector 1
    put_be32(&mut b, 32, !0); // filesystem-header list: none
    put_be32(&mut b, 36, !0); // drive init
    put_be32(&mut b, 40, !0);
    for off in (44..64).step_by(4) {
        put_be32(&mut b, off, !0);
    }
    put_be32(&mut b, 64, total_cyls);
    put_be32(&mut b, 68, 32); // sectors per track (RDB_SPT)
    put_be32(&mut b, 72, 16); // heads (RDB_HEADS)
    put_be32(&mut b, 76, 1); // interleave
    put_be32(&mut b, 80, total_cyls); // park cylinder
    put_be32(&mut b, 96, !0); // write precomp
    put_be32(&mut b, 100, !0); // reduced write
    put_be32(&mut b, 104, 3); // step rate
    put_be32(&mut b, 128, 0); // rdb blocks low
    put_be32(&mut b, 132, CYL_SECTORS - 1); // rdb blocks high
    put_be32(&mut b, 136, 1); // lo cylinder
    put_be32(&mut b, 140, total_cyls - 1); // hi cylinder
    put_be32(&mut b, 144, CYL_SECTORS); // blocks per cylinder
    rdb_checksum(&mut b);
    b
}

/// One bootable partition, `DH0`, spanning cylinders 1..total_cyls-1.
fn build_part_block(total_cyls: u32, dostype: u32) -> Vec<u8> {
    let mut b = vec![0u8; SECTOR_SIZE];
    b[0..4].copy_from_slice(b"PART");
    put_be32(&mut b, 4, 64);
    put_be32(&mut b, 12, 7); // host id
    put_be32(&mut b, 16, !0); // next partition: none
    put_be32(&mut b, 20, 1); // flags: PBFB_BOOTABLE
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
    rdb_checksum(&mut b);
    b
}

/// A built image plus where its one OFS partition starts and how big it is,
/// in sectors -- everything [`find_bootmark`] needs to walk the root
/// directory's own hash table rather than trust a raw byte search (the
/// Startup-Sequence script text itself contains the literal marker string,
/// as the argument to the `Echo` that writes it -- see [`find_bootmark`]'s
/// doc comment).
struct Image {
    bytes: Vec<u8>,
    partition_start_sector: u64,
    partition_sectors: u64,
}

/// A bare OFS partition hardfile -- the host's own `HardDriveImage::open`
/// wraps this in a synthesized RDB (`bare_partition` branch), so the guest
/// mounter only ever sees images with a real RDSK/PART chain.
fn rdbless_image() -> Image {
    let payload = build_ofs_payload();
    assert_eq!(
        payload.len() as u64 % CYL_BYTES,
        0,
        "dirfs::build_image always rounds to a whole 16x32 cylinder"
    );
    assert_eq!(&payload[..3], b"DOS", "bare partition boot block magic");
    let partition_sectors = payload.len() as u64 / SECTOR_SIZE as u64;
    Image {
        bytes: payload,
        partition_start_sector: 0,
        partition_sectors,
    }
}

/// An explicit one-partition RDB (RDSK+PART cylinder 0, `DH0` on cylinders
/// 1..N) wrapping the same OFS payload the RDB-less variant uses.
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

// --- OFS root-directory lookup ------------------------------------------
//
// Mirrors the read side of `src/dirfs.rs`'s `Builder` (hash-table slot
// count, name field offset, root-block placement): the same on-disk layout
// `dirfs::build_image` writes and real AROS's OFS handler reads, so this
// works on a file `Echo` created at runtime just as well as on one
// `dirfs::build_image` built directly.

/// Hash-table / data-pointer entries per header block: 512/4 - 56.
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

/// Standard AmigaDOS (non-international) directory-name hash, matching
/// `dirfs::Builder::dos_hash`.
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

/// Root block of an OFS volume of `partition_sectors` sectors, per
/// `dirfs::Builder::new`'s placement (reserved = 2 blocks, root in the
/// middle of the volume) -- the same formula AROS's own OFS handler derives
/// from the partition's `DosEnvec`. Block numbers inside an OFS volume
/// (this one included) are always partition-relative, so this -- like every
/// other `ofs_*` helper -- takes and returns block numbers relative to the
/// partition's own start, never an absolute file offset.
fn ofs_root_block(partition_sectors: u64) -> u64 {
    (2 + partition_sectors - 1) / 2
}

/// Look `name` up in directory `dir`'s hash table (case-insensitive, as
/// AmigaDOS names are), following the collision chain.
fn ofs_lookup(image: &[u8], dir: u64, name: &[u8]) -> Option<u64> {
    let mut key = u64::from(ofs_get32(image, dir, 24 + ofs_dos_hash(name) * 4));
    while key != 0 {
        if ofs_name(image, key).eq_ignore_ascii_case(name) {
            return Some(key);
        }
        key = u64::from(ofs_get32(image, key, SECTOR_SIZE - 16)); // next_hash
    }
    None
}

/// Read a small (single-data-block) OFS file's content back through its
/// header's data-pointer table. [`MARKER`] fits in one 488-byte OFS data
/// block, so this doesn't need to walk `next_data`/extension chains.
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

/// Whether `image`'s one OFS partition has a root-level file called
/// `bootmark` whose content is [`MARKER`] -- proof AROS mounted the
/// partition, autobooted from it, and ran its Startup-Sequence to
/// completion, found structurally rather than by searching the image's raw
/// bytes for the marker string.
///
/// A raw byte search would be a false positive independent of whether any
/// of that actually happened: the Startup-Sequence itself (`S/Startup-
/// Sequence`, written into the image before it is ever booted) contains the
/// literal text `Echo >"SYS:bootmark" "COPPERHF-M3-BOOTED"` -- the marker
/// string is right there in the script that is supposed to produce it, so
/// `image_bytes.windows(MARKER.len()).any(...)` finds it whether or not
/// the mounter, or AROS, ever runs at all.
///
/// Works on the partition's own block-relative slice of the image: every
/// pointer inside an OFS volume (hash-table entries, `next_hash`, data
/// pointers) is a partition-relative block number, not an absolute file
/// offset, so the RDB variant's partition (which starts a whole synthesized
/// cylinder into the file) has to be addressed the same way a real AROS
/// mount would -- relative to `de_LowCyl`, not relative to block 0 of the
/// file.
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

// --- Test harness ------------------------------------------------------

fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "copperline-copperhf-mounter-{name}-{}",
        std::process::id()
    ))
}

/// Boot the bundled AROS ROM with `image` attached as `[copperhf] unit0`
/// (no other disks, no `--run` staging), then check the image for the
/// `bootmark` file (see [`find_bootmark`]), which only lands if AROS
/// actually mounted the unit's partition, autobooted from it, and ran its
/// Startup-Sequence.
fn run_mounter_test(tag: &str, image: &Image) {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping copperhf.device M3 mounter test ({tag}); run with --release \
             (a debug emulator is far too slow for a full AROS autoboot)"
        );
        return;
    }

    let root = repo_root();
    let scratch = scratch_dir(tag);
    let _ = std::fs::remove_dir_all(&scratch);
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&config_home).unwrap();

    let image_path = scratch.join("unit0.hdf");
    std::fs::File::create(&image_path)
        .unwrap()
        .write_all(&image.bytes)
        .unwrap();

    let config = scratch.join("copperhf.toml");
    std::fs::write(
        &config,
        format!(
            "rom = \"<bundled-aros>\"\n\n[copperhf]\nunit0 = {}\n",
            toml_path(&image_path)
        ),
    )
    .unwrap();

    let screenshot = scratch.join("copperhf.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&root)
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--config")
        .arg(&config)
        .arg("--noaudio")
        // Generous budget: a bundled-AROS autoboot to a Startup-Sequence
        // over a real mounted hard-disk partition (rather than the `--run`
        // staging volume tests/copperhf_device.rs boots) is slower than the
        // ~11s DF0 boot, and this milestone's mounter is new code.
        .arg("--screenshot-after")
        .arg("60")
        .arg(&screenshot)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "[{tag}] Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    let bytes = std::fs::read(&image_path).unwrap();
    let found_image = Image {
        bytes,
        partition_start_sector: image.partition_start_sector,
        partition_sectors: image.partition_sectors,
    };
    assert!(
        find_bootmark(&found_image),
        "[{tag}] bootmark file absent (or wrong content) after boot -- AROS never mounted \
         unit 0's partition, never autobooted from it, or the Startup-Sequence never ran:\n\
         stdout:\n{}\nstderr:\n{}",
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    std::fs::remove_dir_all(&scratch).ok();
}

#[test]
fn aros_autoboots_from_rdb_image() {
    run_mounter_test("rdb", &rdb_image());
}

#[test]
fn aros_autoboots_from_rdbless_image() {
    run_mounter_test("rdbless", &rdbless_image());
}

/// [`find_bootmark`] must be a structural check, not a raw byte search: the
/// Startup-Sequence script itself contains the literal marker text (as the
/// argument to the `Echo` that is supposed to write it), so a byte search
/// would report success on a pristine, never-booted image. Both image
/// builders are exercised directly here (not through a full emulator boot)
/// so this stays fast and always runs, unlike the release-only, boot-driven
/// tests above.
#[test]
fn find_bootmark_is_false_on_a_pristine_unbooted_image() {
    assert!(!find_bootmark(&rdbless_image()));
    assert!(!find_bootmark(&rdb_image()));
}
