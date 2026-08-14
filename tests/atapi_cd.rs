// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end ATAPI regression: boot a `[lide]` Zorro II board carrying
//! LIV2's real `cdfs.rom` filesystem driver, with a genuine ISO9660 image
//! attached as the channel's ATAPI slave, and confirm the real third-party
//! firmware's own probe sequence succeeds against Copperline's PACKET
//! (0xA0) protocol implementation: IDENTIFY DEVICE aborts on the ATAPI
//! slot (as it must, so the driver knows to retry with IDENTIFY PACKET
//! DEVICE), IDENTIFY PACKET DEVICE succeeds, and `cdfs.rom` goes on to read
//! real sectors (the ISO9660 Primary Volume Descriptor at LBA 16) through
//! PACKET data-in transfers.
//!
//! This is a firmware-compatibility check, not a guest-OS boot check: no
//! bundled Amiga OS ships ATAPI support in its stock `ide.device`/
//! `scsi.device` (see AGENTS.md), so there is no way to prove "AmigaOS
//! mounts the CD" without also bundling a period driver as a test asset.
//! `cdfs.rom` is exactly that driver, and it is real hardware firmware, not
//! project code -- so this test reads the emulator's own diagnostic log
//! for the expected command sequence rather than asserting on guest-visible
//! state, and skips cleanly wherever the asset or an ISO-authoring tool is
//! unavailable, per tests/README.md's asset contract.
//!
//! The protocol-level contract itself (chunked PIO, the interrupt-reason
//! register, error/sense mapping, mixed disk+ATAPI buses) is covered by
//! the deterministic unit tests in `src/ata.rs`; this test exists to catch
//! anything those miss because they use a synthetic host, not a real one.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Local ROM/disk assets: `COPPERLINE_TEST_ASSETS`, else `test-assets/`,
/// else the repo root (tests/README.md).
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

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "copperline-atapi-cd-test-{}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Escape a path for inclusion in a double-quoted TOML string.
fn toml_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Build a minimal ISO9660 image with one file, `PROBE.TXT`, using whatever
/// ISO-authoring tool is on the host. Returns `None` (the caller skips) if
/// none is available -- this is a test-tooling gap, not a missing asset,
/// but it skips the same way for the same reason: nothing to test against.
fn build_probe_iso(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("isosrc");
    std::fs::create_dir_all(&src).ok()?;
    std::fs::write(src.join("PROBE.TXT"), b"hello from the atapi cd\n" as &[u8]).ok()?;
    let iso = dir.join("probe.iso");

    if Command::new("hdiutil").arg("--help").output().is_ok() {
        let status = Command::new("hdiutil")
            .args(["makehybrid", "-iso", "-joliet", "-o"])
            .arg(&iso)
            .arg(&src)
            .status()
            .ok()?;
        if status.success() && iso.is_file() {
            return Some(iso);
        }
    }
    for tool in ["genisoimage", "mkisofs"] {
        if Command::new(tool).arg("--version").output().is_ok() {
            let status = Command::new(tool)
                .args(["-quiet", "-o"])
                .arg(&iso)
                .arg(&src)
                .status()
                .ok()?;
            if status.success() && iso.is_file() {
                return Some(iso);
            }
        }
    }
    None
}

/// Boot a `[lide]` "ride" board (one ATA channel) with a hard disk on the
/// master slot and the ATAPI CD-ROM on the slave slot, capture its
/// diagnostic log, and return it alongside the screenshot path.
fn boot_lide_atapi(
    name: &str,
    lide_rom: &Path,
    cdfs_bank: &Path,
    hdf: &Path,
    iso: &Path,
) -> Result<(String, PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let temp = temp_dir(name);
    let cfg_path = temp.join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            r#"
[machine]
profile = "A600"

[lide]
board = "ride"
rom = "{rom}"
rom_bank2 = "{bank2}"
drives = ["{hdf}", "{iso}"]
"#,
            rom = toml_path(lide_rom),
            bank2 = toml_path(cdfs_bank),
            hdf = toml_path(hdf),
            iso = toml_path(iso),
        ),
    )?;

    let png = temp.join("boot.png");
    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .env("RUST_LOG", "copperline=info")
        // The `ide cmd ...` trace this test asserts on is itself gated
        // behind this diagnostic flag (src/ata.rs), not just RUST_LOG.
        .env("COPPERLINE_DIAG_GAYLE", "1")
        .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
        .arg("--config")
        .arg(&cfg_path)
        .arg("--noaudio")
        .arg("--screenshot-after")
        .arg("30")
        .arg(&png)
        .output()?;
    if !output.status.success() {
        panic!(
            "copperline exited with {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(png.is_file(), "screenshot was not written");
    let log = String::from_utf8_lossy(&output.stderr).into_owned();
    Ok((log, png, temp))
}

/// Needs: a local `[lide]` `rom` (32768-byte lide.device-compatible board
/// ROM) and `rom_bank2`-shaped `cdfs.rom` (LIV2's real CD filesystem
/// driver) under the asset directory (see tests/README.md), a hard disk
/// image to boot the machine's ATA master from, and a host ISO-authoring
/// tool (`hdiutil` on macOS, `genisoimage`/`mkisofs` elsewhere). Skips
/// cleanly when any of these is unavailable.
#[test]
#[ignore = "runs the emulator and requires local lide ROM/cdfs.rom assets"]
fn atapi_cd_answers_a_real_lide_device_driver_probe() -> Result<(), Box<dyn std::error::Error>> {
    let dir = asset_dir();
    let lide_rom = dir.join("lide/lide.rom");
    let cdfs_src = dir.join("lide/cdfs.rom");
    let hdf = dir.join("lide/work.hdf");
    if !lide_rom.is_file() || !cdfs_src.is_file() || !hdf.is_file() {
        eprintln!(
            "skipping ATAPI regression; missing one of {}, {}, {}",
            lide_rom.display(),
            cdfs_src.display(),
            hdf.display()
        );
        return Ok(());
    }

    let temp = temp_dir("atapi-cd");
    let Some(iso) = build_probe_iso(&temp) else {
        eprintln!("skipping ATAPI regression; no ISO-authoring tool (hdiutil/genisoimage/mkisofs) on this host");
        return Ok(());
    };
    // load_rom pads a short dump out to a full bank itself, so cdfs_src can
    // be handed to [lide] rom_bank2 as-is.
    let (log, _png, boot_temp) = boot_lide_atapi("probe", &lide_rom, &cdfs_src, &hdf, &iso)?;

    // The real driver's probe: IDENTIFY DEVICE (0xEC) against the ATAPI
    // slot must abort (drv=1 is the CD-ROM slave here) so the driver falls
    // back to IDENTIFY PACKET DEVICE (0xA1), which must then succeed and
    // be followed by real PACKET (0xA0) data transfers -- this is what
    // `cdfs.rom` actually does on real hardware, and it is what its own
    // command trace should show here if our task-file/PACKET emulation is
    // wire-compatible with a real driver rather than only with our own unit
    // tests.
    //
    // NOTE: `ide cmd ... drv=1` alone does not identify which controller
    // (Gayle's own empty slave vs. this lide board) emitted it -- an
    // ordinary AROS probe of an empty Gayle slave on the same A600 profile
    // logs an identical line. Tagging the trace with the emitting board
    // would need plumbing an identity through `AtaBus`, which every board
    // (Gayle, the A4000's own controller, lide) shares; that's more
    // restructuring than this regression warrants, so the assertion below
    // instead proves the *content* is right: a real PACKET-driven READ
    // actually returned the ISO9660 Primary Volume Descriptor at LBA 16,
    // which only `cdfs.rom` reading real sectors through this lide board
    // would produce. The board-identity ambiguity on the `ide cmd` lines
    // remains a known limitation.
    assert!(
        log.contains("ide cmd 0xEC drv=1"),
        "driver never probed the ATAPI slot with IDENTIFY DEVICE:\n{log}"
    );
    assert!(
        log.contains("ide cmd 0xA1 drv=1"),
        "driver never followed up with IDENTIFY PACKET DEVICE:\n{log}"
    );
    assert!(
        log.contains("ide cmd 0xA0 drv=1"),
        "driver never issued a PACKET command to the ATAPI slot:\n{log}"
    );
    assert!(
        !log.contains("IDE: unimplemented command"),
        "an ATA/ATAPI command the driver issued was not recognized:\n{log}"
    );
    // The PACKET-level trace (src/ata.rs's packet_command_received) proves
    // not just that *a* PACKET command was issued, but that a READ(10)
    // (opcode 0x28) actually targeted LBA 16 -- the ISO9660 Primary Volume
    // Descriptor every driver reads first to identify the filesystem.
    assert!(
        log.contains("ide packet cdb drv=1 op=0x28 lba=16"),
        "driver never issued READ(10) for the ISO9660 PVD at LBA 16:\n{log}"
    );
    // The issue-time trace above only proves the READ(10) was asked for --
    // not that it actually succeeded or returned the real disc content a
    // regression could otherwise silently drop or corrupt. The result-time
    // trace (logged once ScsiCdRom::execute has actually run) must show a
    // GOOD status and the real ISO9660 "CD001" signature bytes, proving
    // genuine sector data made the round trip, not just a status byte.
    assert!(
        log.contains("ide packet result drv=1 data_in") && log.contains("pvd_signature=true"),
        "READ(10) never returned a successful, PVD-signed response:\n{log}"
    );

    std::fs::remove_dir_all(&temp).ok();
    std::fs::remove_dir_all(&boot_temp).ok();
    Ok(())
}
