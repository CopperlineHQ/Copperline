//! Golden-render regression tests for the timing-test probe suite.
//!
//! Each probe is a self-contained bootblock program (timing-test/*.asm,
//! committed alongside its assembled .bin) that takes over the machine and
//! draws a display exercising one calibrated hardware behaviour. The test
//! assembles each probe into a bootable ADF in a temp directory, boots it on
//! the bundled AROS ROM (no Kickstart needed -- the probes own the machine
//! once loaded, so the settled frame of the display-geometry probes is
//! ROM-independent), captures a raw screenshot at a fixed emulated time, and
//! compares it pixel-for-pixel against the committed reference under
//! `timing-test/golden/`.
//!
//! Exception: the two probes that render live E-clock-referenced counts
//! (`timing-test`, `bltprobe-pace`) do depend on the instant the ROM hands
//! control to the bootblock -- the E-clock and DMA-cadence phase relative to
//! the beam at probe start varies with boot duration, shifting the displayed
//! counts by a tick or two. Refreshing the bundled AROS ROM therefore moves
//! those two goldens and requires a re-bless.
//!
//! The emulator core is deterministic, so any pixel difference is a real
//! behaviour change. When a change is intentional (a hardware-model fix that
//! moves calibrated output, or a bundled-ROM refresh), re-bless the goldens
//! and review the diff in the commit:
//!
//! ```sh
//! COPPERLINE_BLESS_GOLDEN=1 cargo test --release --test probe_golden
//! ```
//!
//! On mismatch the actual render and a diff mask are written under
//! `target/probe-golden/` (uploaded as artifacts by CI).
//!
//! The suite runs release-only: `cargo test` in the debug profile skips it
//! (a debug-build emulator is far too slow for the ~16s emulated boots).
//!
//! Probes excluded by design: ddfprobe-cc5/-cc6 sit on deliberate race
//! boundaries (they demonstrate launch-phase bistability and free-running
//! precession, so any unrelated timing change flips them), and ddfprobe-cc7
//! replays a chip-RAM dump of a running demo that is not committed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ADF_SIZE: usize = 901_120; // 80 tracks * 2 sides * 11 sectors * 512
const BOOTBLOCK_SIZE: usize = 1024;

/// The machine a probe boots on. Every probe runs the stock A500 shape
/// (68000, 512K chip + 512K slow, PAL); the chipset revision varies where a
/// probe's reference measurements were taken on ECS (the DDFSTRT comparator
/// has 2-cck resolution on ECS vs 4-cck on OCS, so phase probes differ).
#[derive(Clone, Copy)]
enum Machine {
    Ocs,
    Ecs,
    /// A1200 shape (68EC020, AGA, 2M chip) for probes of AGA-only
    /// behaviour (FMODE wide fetches, extended BPLCON1 scroll).
    Aga,
}

/// One golden-render probe: name, main-program binary, emulated seconds
/// before the shot, and the machine it boots on.
/// The AROS bootstrap hands control to the bootblock at ~11s emulated
/// (boot-time-optimized ROM, see assets/aros/README.md; it was ~32s before);
/// every display probe is settled and static by 16s (ddfprobe matches its
/// golden from 12s on, verified 16 == 20), and the timing test has printed
/// all its measurement rows by 32s (verified 30 == 44).
struct Probe {
    name: &'static str,
    program: &'static str,
    seconds: f64,
    machine: Machine,
}

const fn probe(name: &'static str, program: &'static str, seconds: f64) -> Probe {
    Probe {
        name,
        program,
        seconds,
        machine: Machine::Ocs,
    }
}

const fn probe_ecs(name: &'static str, program: &'static str, seconds: f64) -> Probe {
    Probe {
        name,
        program,
        seconds,
        machine: Machine::Ecs,
    }
}

const fn probe_aga(name: &'static str, program: &'static str, seconds: f64) -> Probe {
    Probe {
        name,
        program,
        seconds,
        machine: Machine::Aga,
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Standard Amiga boot-block checksum: the end-around-carry sum of every
/// longword (with the checksum field zeroed) one's-complemented, so the
/// block sums to $FFFFFFFF. Mirrors timing-test/make_adf.py.
fn boot_checksum(block: &[u8]) -> u32 {
    let mut total: u64 = 0;
    for i in (0..BOOTBLOCK_SIZE).step_by(4) {
        if i == 4 {
            continue;
        }
        let word = u32::from_be_bytes(block[i..i + 4].try_into().unwrap());
        total += u64::from(word);
        total = (total & 0xFFFF_FFFF) + (total >> 32);
    }
    !(total as u32)
}

/// Wrap the committed boot block and a probe's main program into a bootable
/// ADF image (sector 0-1: boot block, sector 2+: the program the boot block
/// loads to chip RAM and jumps to).
fn build_adf(boot: &[u8], program: &[u8]) -> Vec<u8> {
    assert!(boot.len() <= BOOTBLOCK_SIZE, "boot block too large");
    assert!(
        BOOTBLOCK_SIZE + program.len() <= ADF_SIZE,
        "program does not fit on the disk"
    );
    let mut block = vec![0u8; BOOTBLOCK_SIZE];
    block[..boot.len()].copy_from_slice(boot);
    let checksum = boot_checksum(&block);
    block[4..8].copy_from_slice(&checksum.to_be_bytes());

    let mut image = vec![0u8; ADF_SIZE];
    image[..BOOTBLOCK_SIZE].copy_from_slice(&block);
    image[BOOTBLOCK_SIZE..BOOTBLOCK_SIZE + program.len()].copy_from_slice(program);
    image
}

fn probe_config(adf: &Path, machine: Machine) -> String {
    let chipset = match machine {
        Machine::Ocs => "revision = \"OCS\"\n",
        Machine::Ecs => "revision = \"ECS\"\nagnus = \"8372A\"\ndenise = \"OCS\"\n",
        Machine::Aga => "revision = \"AGA\"\n",
    };
    let (cpu, memory) = match machine {
        Machine::Ocs | Machine::Ecs => ("68000", "chip = \"512K\"\nslow = \"512K\"\n"),
        Machine::Aga => ("68EC020", "chip = \"2M\"\n"),
    };
    format!(
        "rom = \"<bundled-aros>\"\n\
         [display]\n\
         overscan = \"full\"\n\
         [cpu]\n\
         model = \"{cpu}\"\n\
         [memory]\n\
         {memory}\
         [chipset]\n\
         {chipset}\
         video = \"PAL\"\n\
         [floppy.df0]\n\
         path = \"{}\"\n\
         write_protected = true\n",
        adf.display()
    )
}

struct Rgba {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn load_png(path: &Path) -> Result<Rgba, Box<dyn std::error::Error>> {
    let decoder = png::Decoder::new(fs::File::open(path)?);
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "{}", path.display());
    buf.truncate(info.buffer_size());
    Ok(Rgba {
        width: info.width,
        height: info.height,
        data: buf,
    })
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) {
    let file = fs::File::create(path).expect("create diff png");
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
}

/// Render a probe and return the path of the captured screenshot.
fn capture_probe(p: &Probe, work: &Path) -> PathBuf {
    let name = p.name;
    let program = p.program;
    let seconds = p.seconds;
    let root = repo_root();
    let boot = fs::read(root.join("timing-test/boot.bin")).expect("timing-test/boot.bin");
    let main = fs::read(root.join("timing-test").join(program))
        .unwrap_or_else(|e| panic!("timing-test/{program}: {e}"));
    let adf_path = work.join(format!("{name}.adf"));
    fs::write(&adf_path, build_adf(&boot, &main)).expect("write adf");
    let cfg_path = work.join(format!("{name}.toml"));
    fs::write(&cfg_path, probe_config(&adf_path, p.machine)).expect("write config");
    let shot_path = work.join(format!("{name}.png"));

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&root)
        // The raw (unblended, line-doubled) framebuffer with recentring off:
        // the calibrated forensic capture mode, byte-stable run to run.
        .env("COPPERLINE_HCENTER", "0")
        .env("COPPERLINE_SHOT_RAW", "1")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("RUST_LOG", "copperline=warn")
        .arg("--noaudio")
        .arg("--config")
        .arg(&cfg_path)
        .arg("--screenshot-after")
        .arg(format!("{seconds}"))
        .arg(&shot_path)
        .output()
        .expect("run emulator");
    assert!(
        output.status.success(),
        "{name}: emulator exited with {}\nstderr tail:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(shot_path.exists(), "{name}: no screenshot was written");
    shot_path
}

/// Capture one probe and compare it against its committed golden (or bless
/// it when `COPPERLINE_BLESS_GOLDEN` is set). Each probe is its own `#[test]`
/// (see the macro below), so the libtest harness runs the emulator boots in
/// parallel on the available cores.
fn run_probe(p: &Probe) {
    if cfg!(debug_assertions) {
        eprintln!("skipping probe goldens; run with --release (a debug emulator is too slow)");
        return;
    }

    let name = p.name;
    let root = repo_root();
    let golden_dir = root.join("timing-test/golden");
    let bless = std::env::var_os("COPPERLINE_BLESS_GOLDEN").is_some();
    let work = std::env::temp_dir().join(format!(
        "copperline-probe-golden-{}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&work).expect("create work dir");
    let diff_dir = root.join("target/probe-golden");

    let shot_path = capture_probe(p, &work);
    let golden_path = golden_dir.join(format!("{name}.png"));

    if bless {
        fs::create_dir_all(&golden_dir).expect("create golden dir");
        fs::copy(&shot_path, &golden_path).expect("bless golden");
        eprintln!("blessed {}", golden_path.display());
        let _ = fs::remove_dir_all(&work);
        return;
    }

    let mut failure = None;
    if !golden_path.exists() {
        failure = Some(format!(
            "{name}: missing golden {} (generate with COPPERLINE_BLESS_GOLDEN=1)",
            golden_path.display()
        ));
    } else {
        let golden = load_png(&golden_path).expect("load golden");
        let actual = load_png(&shot_path).expect("load capture");
        if (golden.width, golden.height) != (actual.width, actual.height) {
            failure = Some(format!(
                "{name}: geometry changed {}x{} -> {}x{}",
                golden.width, golden.height, actual.width, actual.height
            ));
        } else {
            let differing = golden
                .data
                .chunks_exact(4)
                .zip(actual.data.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count();
            if differing > 0 {
                fs::create_dir_all(&diff_dir).expect("create diff dir");
                let actual_out = diff_dir.join(format!("{name}-actual.png"));
                fs::copy(&shot_path, &actual_out).expect("copy actual");
                // Diff mask: white where pixels differ, black elsewhere.
                let mask: Vec<u8> = golden
                    .data
                    .chunks_exact(4)
                    .zip(actual.data.chunks_exact(4))
                    .flat_map(|(a, b)| {
                        if a == b {
                            [0, 0, 0, 255]
                        } else {
                            [255, 255, 255, 255]
                        }
                    })
                    .collect();
                write_png(
                    &diff_dir.join(format!("{name}-diff.png")),
                    golden.width,
                    golden.height,
                    &mask,
                );
                failure = Some(format!(
                    "{name}: {differing} of {} pixels differ from timing-test/golden/{name}.png \
                     (actual + diff mask in target/probe-golden/)",
                    (golden.width * golden.height)
                ));
            }
        }
    }

    let _ = fs::remove_dir_all(&work);
    assert!(
        failure.is_none(),
        "probe golden mismatch:\n  {}\n\nIf the change is an intentional hardware-model \
         fix, re-bless with COPPERLINE_BLESS_GOLDEN=1 cargo test --release --test \
         probe_golden and review the render diff in the commit.",
        failure.unwrap()
    );
}

/// One `#[test]` per probe so the harness parallelises the emulator boots
/// (the shared work is per-probe; there is no cross-probe state).
macro_rules! probe_tests {
    ($($test_name:ident => $p:expr;)*) => {
        $(
            #[test]
            fn $test_name() {
                run_probe(&$p);
            }
        )*
    };
}

probe_tests! {
    golden_timing_test => probe("timing-test", "test.bin", 32.0);
    golden_ddfprobe => probe("ddfprobe", "ddfprobe.bin", 16.0);
    golden_ddfprobe_diw1 => probe("ddfprobe-diw1", "ddfprobe-diw1.bin", 16.0);
    golden_ddfprobe_toggle => probe("ddfprobe-toggle", "ddfprobe-toggle.bin", 16.0);
    golden_ddfprobe_cc => probe("ddfprobe-cc", "ddfprobe-cc.bin", 16.0);
    golden_ddfprobe_cc3 => probe("ddfprobe-cc3", "ddfprobe-cc3.bin", 16.0);
    golden_ddfprobe_cc4 => probe("ddfprobe-cc4", "ddfprobe-cc4.bin", 16.0);
    golden_ddfprobe_sprbar => probe("ddfprobe-sprbar", "ddfprobe-sprbar.bin", 16.0);
    golden_ddfprobe_sprbar2 => probe("ddfprobe-sprbar2", "ddfprobe-sprbar2.bin", 16.0);
    golden_ddfprobe_sotb => probe("ddfprobe-sotb", "ddfprobe-sotb.bin", 16.0);
    golden_ddfprobe_sotb2 => probe("ddfprobe-sotb2", "ddfprobe-sotb2.bin", 16.0);
    // DDFSTRT sub-unit phase / BPLCON1 scroll placement maps, ECS-verified
    // against vAmiga (the Rampage dot-cube pan regression class).
    golden_ddfprobe_phase => probe_ecs("ddfprobe-phase", "ddfprobe-phase.bin", 16.0);
    golden_ddfprobe_phase2 => probe_ecs("ddfprobe-phase2", "ddfprobe-phase2.bin", 16.0);
    // BPLCON1 hi-res scroll placement map on the Kickstart 2.05 boot-screen
    // constellation (late DDF, narrow DIW): one lo-res pixel = 2 hi-res px
    // per scroll step, nibble bit 3 ignored, row-end overlap words clipped
    // at the DIW stop (the KS 2.05 first-text-column regression class);
    // vAmiga-verified band by band.
    golden_ddfprobe_hscroll => probe_ecs("ddfprobe-hscroll", "ddfprobe-hscroll.bin", 16.0);
    // AGA wide-FMODE off-grid DDFSTRT scroll fold: taps in the last
    // `earliness` px of the gulp window show the next gulp, one gulp left
    // (the Alien Breed II AGA horizontal-scroll regression class, issue
    // #248); FS-UAE-verified band by band.
    golden_ddfprobe_agafold => probe_aga("ddfprobe-agafold", "ddfprobe-agafold.bin", 16.0);
    // FMODE.SSCAN2 masks the sprite horizontal comparator's high bit:
    // HSTART $165 aliases $065 while $080 remains distinct (the DblPAL
    // High Res Laced invisible-pointer regression class, issue #270);
    // FS-UAE-verified exact placement.
    golden_dblpal_hires_lace => probe_aga("dblpal-hires-lace", "dblpal-hires-lace.bin", 16.0);
    // CPU pacing bars under BLTPRI copy/fill/line blits (the Rampage
    // "present" flicker / BLS fence regression class).
    golden_bltprobe_pace => probe("bltprobe-pace", "bltprobe-pace.bin", 16.0);
    // DMA sprite vertical reuse + attached pair placement (the sprite
    // register-FSM regression class).
    golden_ddfprobe_sprmulti => probe("ddfprobe-sprmulti", "ddfprobe-sprmulti.bin", 16.0);
    // CPU byte writes to a custom register latch the mirrored word (the
    // COLOR00 byte-write regression class).
    golden_regprobe_bytemirror => probe("regprobe-bytemirror", "regprobe-bytemirror.bin", 16.0);
    // CLXDAT collision matrix bits rendered as cells (the collision
    // matching/enable regression class).
    golden_clxprobe => probe("clxprobe", "clxprobe.bin", 16.0);
    // AUD0 interrupt cadence strip across a scripted AUDxEN
    // enable/punch/disable/restart sequence (the issue #74 deferred
    // AUDxEN-disable regression class).
    golden_audprobe_en => probe("audprobe-en", "audprobe-en.bin", 16.0);
    // Sprite DMA fetches land in the Denise display latches: the terminator
    // CTL fetch disarms, DATA/DATB fetches overwrite, and a later bare
    // SPRxDATA arm redisplays the DMA-written words (the Hamazing
    // scene-switch stale-bar regression class).
    golden_sprprobe_latch => probe("sprprobe-latch", "sprprobe-latch.bin", 16.0);
    // A SPRxCTL write between a DMA fetch slot and that channel's HSTART
    // disarms Denise before the serializer loads, cancelling the fetched
    // line; a write past HSTART cannot recall it (the Hybris panel
    // stray-dash regression class, issue #278). vAmiga-verified.
    golden_sprprobe_disarm => probe("sprprobe-disarm", "sprprobe-disarm.bin", 16.0);
    // BPLCON0's HAM select reaches Denise in the colour-selection phase, so a
    // mid-line HAM change lands where a COLORxx write carried by the same
    // chip-bus slot would: eight bands clear HAM 16 colour clocks apart and
    // the blue/green staircase reads off the landing column (the Hollywood
    // Poker Pro HAM-photo/EHB-scoreboard split-line regression class).
    // vAmiga-verified: byte-identical over the whole frame.
    golden_hamprobe_select => probe("hamprobe-select", "hamprobe-select.bin", 16.0);
}
