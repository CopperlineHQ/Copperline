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
//! Exception: ten probes depend on the state the ROM hands to the
//! bootblock. The two that render live E-clock-referenced counts
//! (`timing-test`, `bltprobe-pace`) see the E-clock and DMA-cadence phase
//! relative to the beam vary with boot duration, shifting the displayed
//! counts by a tick or two. `a500-cia-request` records beam positions after
//! E-clock-synchronised CIA accesses, and its ICR samples include CIA-A
//! timer B, which the probe never programs and the ROM leaves running.
//! `jit-bus-boundary` draws markers whose position against the beam also
//! follows the handoff instant. Six more measure from wherever they start
//! against the beam: `bltprobe-post` (blit polls left, against the display
//! fetch), `posprobe-v8` (frame wraps spanned), `a500-display-boundaries`
//! (raw beam samples), and `irqprobe-window`, `memprobe-chipmirror` and
//! `memprobe-chipmirror-ecs` (CPU-painted stripe edges). Refreshing the
//! bundled AROS ROM can move any of these ten goldens and requires a
//! re-bless; timing-test/README.md ("CI golden renders") records how each
//! move was checked.
//!
//! The emulator core is deterministic, so any pixel difference is a real
//! behaviour change. When a change is intentional (a hardware-model fix that
//! moves calibrated output, or a bundled-ROM refresh), re-bless the goldens
//! and review the diff in the commit:
//!
//! ```sh
//! COPPERLINE_BLESS_GOLDEN=ddfprobe cargo test --release --test probe_golden golden_ddfprobe -- --exact
//! ```
//!
//! On mismatch the actual render and a diff mask are written under
//! `target/probe-golden/` (uploaded as artifacts by CI).
//!
//! The suite runs optimized-only: `cargo test` in the debug profile ignores it
//! (a debug-build emulator is far too slow for the ~16s emulated boots).
//!
//! Probes excluded by design: ddfprobe-cc5/-cc6 sit on deliberate race
//! boundaries (they demonstrate launch-phase bistability and free-running
//! precession, so any unrelated timing change flips them), and ddfprobe-cc7
//! replays a chip-RAM dump of a running demo that is not committed.

mod common;

#[allow(dead_code)]
#[path = "../src/envcfg.rs"]
mod envcfg;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const ADF_SIZE: usize = 901_120; // 80 tracks * 2 sides * 11 sectors * 512
const BOOTBLOCK_SIZE: usize = 1024;

/// The machine a probe boots on. Every probe runs the stock A500 shape
/// (68000, 512K chip + 512K slow, PAL); the chipset revision varies where a
/// probe's reference measurements were taken on ECS (the DDFSTRT comparator
/// has 2-cck resolution on ECS vs 4-cck on OCS, so phase probes differ).
#[derive(Clone, Copy)]
enum Machine {
    /// Fixed-clock JIT accelerator with local fast RAM.
    Jit,
    Ocs,
    /// The OCS A500 shape with a 68010 fitted, for probes of that
    /// processor's own bus timing.
    Ocs68010,
    /// ECS Agnus (8372A) beside an OCS Denise, the late-A500 pairing.
    Ecs,
    /// ECS Agnus beside an ECS Denise (8373), for probes of the Denise
    /// side of ECS: BPLCON3, KILLEHB, programmable blanking.
    EcsDenise,
    /// A1200 shape (68EC020, AGA, 2M chip) for probes of AGA-only
    /// behaviour (FMODE wide fetches, extended BPLCON1 scroll).
    Aga,
    /// A1200 profile exercising NTSC field geometry.
    AgaNtsc,
    /// A1200 profile with the motherboard peripherals and PAL timing: the
    /// Gayle address decode and the Alice blitter final D write.
    A1200,
    A4000,
    A4000Buster,
    /// A3000 profile: ECS, 68030, Fat Gary with the A3000's ROM jumpers.
    A3000,
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

// The ADF path is written as a TOML literal (single-quoted) string so a
// Windows temp path's backslashes are not parsed as escape sequences.
fn probe_config(adf: &Path, machine: Machine) -> String {
    let chipset = match machine {
        Machine::Jit | Machine::A3000 => "revision = \"ECS\"\n",
        Machine::Ocs | Machine::Ocs68010 => "revision = \"OCS\"\n",
        Machine::Ecs => "revision = \"ECS\"\nagnus = \"8372A\"\ndenise = \"OCS\"\n",
        Machine::EcsDenise => "revision = \"ECS\"\nagnus = \"8372A\"\ndenise = \"ECS\"\n",
        Machine::Aga
        | Machine::AgaNtsc
        | Machine::A1200
        | Machine::A4000
        | Machine::A4000Buster => "revision = \"AGA\"\n",
    };
    let (cpu, memory) = match machine {
        Machine::A4000 | Machine::A4000Buster => ("68040", "chip = \"2M\"\n"),
        Machine::A3000 => ("68030", "chip = \"2M\"\n"),
        Machine::Jit => ("68040", "chip = \"2M\"\nfast = \"8M\"\n"),
        Machine::Ocs | Machine::Ecs | Machine::EcsDenise => {
            ("68000", "chip = \"512K\"\nslow = \"512K\"\n")
        }
        Machine::Ocs68010 => ("68010", "chip = \"512K\"\nslow = \"512K\"\n"),
        Machine::Aga | Machine::AgaNtsc | Machine::A1200 => ("68EC020", "chip = \"2M\"\n"),
    };
    let acceleration = if matches!(machine, Machine::Jit) {
        "jit = true\nclock_mhz = 25\n"
    } else {
        ""
    };
    let (profile, video) = if matches!(
        machine,
        Machine::A4000 | Machine::A4000Buster | Machine::A3000
    ) {
        let scsi = if matches!(machine, Machine::A4000Buster) {
            "[scsi]\ncontroller = \"a4091\"\n"
        } else {
            ""
        };
        (
            format!(
                "[machine]\nprofile = \"{}\"\nbattmem = '{}'\n{scsi}[rtg]\ncard = \"none\"\n",
                if matches!(machine, Machine::A3000) {
                    "A3000"
                } else {
                    "A4000"
                },
                adf.with_extension("nvram").display()
            ),
            "PAL",
        )
    } else if matches!(machine, Machine::AgaNtsc | Machine::A1200) {
        (
            "[machine]\nprofile = \"A1200\"\n[floppy]\ndrives = 1\n".to_owned(),
            if matches!(machine, Machine::AgaNtsc) {
                "NTSC"
            } else {
                "PAL"
            },
        )
    } else {
        (String::new(), "PAL")
    };
    format!(
        "rom = \"<bundled-aros>\"\n\
         {profile}\
         [display]\n\
         overscan = \"full\"\n\
         [cpu]\n\
         model = \"{cpu}\"\n\
         {acceleration}\
         [memory]\n\
         {memory}\
         [chipset]\n\
         {chipset}\
         video = \"{video}\"\n\
         [floppy.df0]\n\
         path = '{}'\n\
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
    let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path)?));
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG dimensions overflow")?;
    let mut buf = vec![0; size];
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

    let output = common::output_with_timeout(
        Command::new(env!("CARGO_BIN_EXE_copperline"))
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
            .arg(&shot_path),
        Duration::from_secs(120),
    )
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
/// it when `COPPERLINE_BLESS_GOLDEN` names this probe). Each probe is its own `#[test]`
/// (see the macro below), so the libtest harness runs the emulator boots in
/// parallel on the available cores.
fn run_probe(p: &Probe) {
    common::require_optimized();

    let name = p.name;
    let root = repo_root();
    let golden_dir = root.join("timing-test/golden");
    let selected = envcfg::var("COPPERLINE_BLESS_GOLDEN");
    if let Some(selected) = &selected {
        assert!(
            PROBES.iter().any(|p| p.name == selected),
            "COPPERLINE_BLESS_GOLDEN must name one probe (for example ddfprobe), got {selected:?}"
        );
    }
    let bless = selected.as_deref() == Some(name);
    let work = std::env::temp_dir().join(format!(
        "copperline-probe-golden-{}-{name}",
        std::process::id()
    ));
    fs::create_dir_all(&work).expect("create work dir");
    let diff_dir = root.join("target/probe-golden");

    let shot_path = capture_probe(p, &work);
    let golden_path = golden_dir.join(format!("{name}.png"));
    let actual = load_png(&shot_path).expect("load capture");
    // The pass/fail probes paint one success colour; the cycle probes draw
    // their measurements instead.
    let pass_fail = matches!(p.machine, Machine::A4000 | Machine::A4000Buster)
        && !matches!(name, "ramsey-cycles" | "fatgary-romcycle");
    if pass_fail || name == "gary-overlay" {
        let center =
            (actual.height as usize / 2 * actual.width as usize + actual.width as usize / 2) * 4;
        assert_eq!(
            &actual.data[center..center + 4],
            &[34, 204, 68, 255],
            "{name}: the guest must finish its checks and paint the success colour"
        );
    }
    if name == "lisa-scanblank" {
        assert_eq!((actual.width, actual.height), (1432, 313));
        let pixel = |x: usize, y: usize| {
            let offset = (y * actual.width as usize + x) * 4;
            &actual.data[offset..offset + 3]
        };
        // CCK $60 plus 3/8 and 5/8, at native 35 ns sample pitch.
        for x in 378..=381 {
            assert_eq!(
                pixel(x, 0),
                if (379..381).contains(&x) {
                    [0, 0, 0]
                } else {
                    [0, 0, 255]
                }
            );
        }
        for row in 0..32 {
            assert_eq!(
                pixel(332, 81 + row),
                if row % 4 < 2 {
                    [255, 0, 0]
                } else {
                    [0, 0, 255]
                }
            );
            assert_eq!(
                pixel(460, 81 + row),
                if row % 2 == 0 {
                    [0, 255, 0]
                } else {
                    [0, 0, 255]
                }
            );
        }
    }
    if name == "diwhigh-fine" {
        assert_eq!((actual.width, actual.height), (1432, 313));
        // First and last yellow column of a row, at the native 35 ns pitch.
        let edges = |y: usize| {
            let row = &actual.data[y * 1432 * 4..(y + 1) * 1432 * 4];
            let lit = |x: usize| row[x * 4..x * 4 + 3] == [255, 255, 0];
            let left = (0..1432).find(|&x| lit(x)).expect("window opens");
            let right = (0..1432).rfind(|&x| lit(x)).expect("window closes");
            (left, right)
        };
        // Row of beam line $40 + 16 * band + 8; the frame starts at line 0.
        let band = |index: usize| edges(0x48 + 16 * index);
        let (left, right) = band(0);
        for fine in 0..4 {
            // Start bits move the left edge only, stop bits the right
            // edge only, one 35 ns column per count.
            assert_eq!(band(fine), (left + fine, right), "start fine {fine}");
            assert_eq!(band(4 + fine), (left, right + fine), "stop fine {fine}");
            assert_eq!(band(8 + fine), (left + fine, right + fine), "both {fine}");
        }
    }
    if name == "regprobe-longwrite" || name == "regprobe-longwrite-68010" {
        assert_eq!((actual.width, actual.height), (716, 570));
        // First and last bar column of a row. A colour clock is four
        // columns at this pitch.
        let edges = |y: usize| {
            let row = &actual.data[y * 716 * 4..(y + 1) * 716 * 4];
            let lit = |x: usize| row[x * 4..x * 4 + 3] != [0x11, 0x11, 0x33];
            let left = (0..716).find(|&x| lit(x)).expect("bar starts");
            let right = (0..716).rfind(|&x| lit(x)).expect("bar ends");
            (left, right)
        };
        // Two rows per beam line and twelve lines per band, from line
        // $50; row 0 is beam line $1C.
        let band = |index: usize| edges(2 * (0x50 - 0x1c + 12 * index + 6));
        let (left, right) = band(0);
        // The first word of a long lands with the word write: by MOVE.L
        // to d16(An) and to an absolute address, from a register and from
        // memory, and by MOVEM.L.
        for first_word in 1..6 {
            assert_eq!(band(first_word), (left, right), "band {}", first_word + 1);
        }
        // The second word lands two colour clocks later, and the second
        // of two word writes six.
        assert_eq!(band(6), (left + 8, right), "second word of a long");
        assert_eq!(band(7), (left + 24, right), "second word write");
        // A predecrement long writes its low word first, so its high word
        // is the one that lands two colour clocks later.
        assert_eq!(band(8), (left + 8, right), "high word to -(An)");
    }
    if matches!(p.machine, Machine::AgaNtsc) {
        // Require the source probe's visible 4:4:2:1 sprite staircase even
        // when blessing, so a failed boot cannot become a blank reference.
        let widths: Vec<_> = actual
            .data
            .chunks_exact(actual.width as usize * 4)
            .map(|row| {
                row.chunks_exact(4)
                    .filter(|pixel| *pixel == [255, 255, 0, 255])
                    .count()
            })
            .filter(|&width| width != 0)
            .collect();
        let expected = [vec![32; 64], vec![16; 32], vec![8; 32]].concat();
        if widths != expected {
            fs::create_dir_all(&diff_dir).expect("create diff dir");
            fs::copy(&shot_path, diff_dir.join(format!("{name}-actual.png"))).expect("copy actual");
        }
        assert_eq!(
            widths, expected,
            "{name}: probe did not render the expected sprite staircase \
             (actual in target/probe-golden/)"
        );
    }

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
            "{name}: missing golden {} (generate with COPPERLINE_BLESS_GOLDEN={name})",
            golden_path.display()
        ));
    } else {
        let golden = load_png(&golden_path).expect("load golden");
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
         fix, re-bless with COPPERLINE_BLESS_GOLDEN={name} cargo test --release --test \
         probe_golden and review the render diff in the commit.",
        failure.unwrap()
    );
}

/// Re-run the green probe's continuous DMA from a saved 68040 machine.
/// A lost ATC entry adds table walks after restore and shifts the A4091's
/// grant even if the screenshot still matches.
#[test]
#[cfg_attr(debug_assertions, ignore = "requires --profile ci or --release")]
fn a4000_replays_ongoing_z3_dma() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name);
    let adf = path("buster.adf");
    let disk = build_adf(
        include_bytes!("../timing-test/boot.bin"),
        include_bytes!("../timing-test/buster-z3-dma.bin"),
    );
    fs::write(&adf, disk).unwrap();
    let cfg = path("a4000.toml");
    fs::write(&cfg, probe_config(&adf, Machine::A4000Buster)).unwrap();
    let mid = path("mid.clstate");
    for (label, resume) in [("continuous", false), ("resumed", true)] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_copperline"));
        cmd.current_dir(repo_root())
            .env("COPPERLINE_AROS_DIR", repo_root().join("assets/aros"))
            .args([
                "--factory",
                "--noaudio",
                "--rtc-time",
                "2001-01-01 00:00:00",
                "--config",
            ])
            .arg(&cfg);
        if resume {
            cmd.arg("--load-state").arg(&mid);
        } else {
            // The guest has verified the buffer and entered the continuous DMA loop.
            cmd.args(["--save-state-after", "7"]).arg(&mid);
        }
        cmd.args(["--save-state-after", "8"])
            .arg(path(&format!("{label}.clstate")))
            .args(["--screenshot-after", "8"])
            .arg(path(&format!("{label}.png")));
        let output = common::output_with_timeout(&mut cmd, Duration::from_secs(120)).unwrap();
        assert!(
            output.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // A boot failure could replay just as deterministically. The green
    // screen is reached only after the guest verifies every copied word.
    let shot = load_png(&path("continuous.png")).unwrap();
    let center = 4 * ((shot.height / 2) * shot.width + shot.width / 2) as usize;
    assert_eq!(&shot.data[center..center + 4], &[0x22, 0xcc, 0x44, 0xff]);
    let expected = fs::read(path("continuous.clstate")).unwrap();
    let resumed = fs::read(path("resumed.clstate")).unwrap();
    let body = copperline::savestate::machine_body_offset;
    assert!(
        expected[body(&expected).unwrap()..] == resumed[body(&resumed).unwrap()..],
        "the machine state diverged after restore"
    );
    assert!(
        fs::read(path("continuous.png")).unwrap() == fs::read(path("resumed.png")).unwrap(),
        "the display diverged after restore"
    );
}

/// One `#[test]` per probe so the harness parallelises the emulator boots
/// (the shared work is per-probe; there is no cross-probe state).
macro_rules! probe_tests {
    ($($test_name:ident => $p:expr;)*) => {
        const PROBES: &[Probe] = &[$($p),*];
        $(
            #[test]
            #[cfg_attr(debug_assertions, ignore = "requires --profile ci or --release")]
            fn $test_name() {
                run_probe(&$p);
            }
        )*
    };
}

probe_tests! {
    golden_buster_z3_dma => Probe { name: "buster-z3-dma", program: "buster-z3-dma.bin", seconds: 7.0, machine: Machine::A4000Buster };
    golden_ntsc_aga_sprites => Probe { name: "ntsc-aga-sprites", program: "agashres-sprites.bin", seconds: 16.0, machine: Machine::AgaNtsc };
    golden_jit_bus_boundary => Probe { name: "jit-bus-boundary", program: "jit-bus-boundary.bin", seconds: 16.0, machine: Machine::Jit };
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
    // The BPLCON1 scroll delay taps pixels the shifter loaded from the same
    // line's pre-window fetch, so the scrolled-in left edge appears on a
    // bitplane-DMA block's first line exactly as on its interior lines --
    // whether the block is gated by a copper DMACON BPLEN write or by
    // DIWSTRT vstart reopening the vertical window (the Super Skidmarks
    // CD32 menu corner-notch regression class); vAmiga-verified exact.
    golden_ddfprobe_blockscroll => probe_ecs("ddfprobe-blockscroll", "ddfprobe-blockscroll.bin", 16.0);
    // AGA wide-FMODE off-grid DDFSTRT scroll fold: taps at or past the
    // data-arrival distance (earliness + the 8-cck fetch-to-comparator
    // pipeline) show the next gulp, one gulp left (the Alien Breed II
    // AGA horizontal-scroll regression class, issue #248);
    // FS-UAE-verified band by band.
    golden_ddfprobe_agafold => probe_aga("ddfprobe-agafold", "ddfprobe-agafold.bin", 16.0);
    // The fold boundary as a function of the DDFSTRT phase on the 64-bit
    // fetch: the boundary saturates past the top of the tap range instead
    // of wrapping (the SANITY Roots II AGA swirl/kaleidoscope regression
    // class, issue #371), and an on-grid start folds from the pipeline
    // alone; FS-UAE-verified band by band.
    golden_ddfprobe_agafold2 => probe_aga("ddfprobe-agafold2", "ddfprobe-agafold2.bin", 16.0);
    // A DDFSTRT below the hardwired $18 start only latches BPHSTART on
    // Alice; the run starts at SHW $18, so the fold phase is the $18 one and
    // every tap renders linearly (the Skeleton Krew CD32 scroll-jump
    // regression class, issue #836); vAmiga-verified band by band.
    golden_ddfprobe_agafold3 => probe_aga("ddfprobe-agafold3", "ddfprobe-agafold3.bin", 16.0);
    // Wide-FMODE's absolute gulp grid remains linear below the standard
    // fetch slots: lo-res BPL64 $18/$B8 hides its whole first 64-px gulp
    // left of a standard DIW and presents the remaining 320 px edge to edge;
    // FS-UAE-verified on the equivalent live display constellation.
    golden_ddfprobe_agaorigin => probe_aga("ddfprobe-agaorigin", "ddfprobe-agaorigin.bin", 16.0);
    // Alice keeps the hardwired $D8 DDF stop at FMODE=0: a lo-res $28/$E0
    // window fetches 23 words, so a -46 modulo rewinds each line to the
    // same row (straight bars); BEAMCON0.HARDDIS in the middle band lifts
    // the stop and the 24-word fetch shears the markers (the Pacmania
    // WHDLoad AGA regression class); FS-UAE/WinUAE-core semantics.
    golden_ddfprobe_agahwstop => probe_aga("ddfprobe-agahwstop", "ddfprobe-agahwstop.bin", 16.0);
    // The DDF start flop sets on the single colour clock where the counter
    // equals DDFSTRT: a write that moves the match behind the beam before
    // the old position fired drops the whole line (the blank middle band),
    // while a write landing after the flop set leaves the run alone (the
    // lower band matches the control band). Restarting from the moved
    // comparator instead desynchronises the planes whose lo-res slot
    // survives the truncated unit (the Microcosm CD32 status-panel
    // regression class).
    golden_ddfprobe_ddfmiss => probe_aga("ddfprobe-ddfmiss", "ddfprobe-ddfmiss.bin", 16.0);
    // A captured OCS run at CCK 0 remains distinct from unset registers.
    golden_ddfprobe_zero => probe("ddfprobe-zero", "ddfprobe-zero.bin", 16.0);
    // Observation baseline: hi-res/SHRES wide-fetch scroll scaling. This
    // pins Copperline output; physical AGA verification is still required.
    golden_ddfprobe_agaresfold => probe_aga("ddfprobe-agaresfold", "ddfprobe-agaresfold.bin", 16.0);
    // FMODE.SSCAN2 masks the sprite horizontal comparator's high bit:
    // HSTART $165 aliases $065 while $080 remains distinct (the DblPAL
    // High Res Laced invisible-pointer regression class, issue #270);
    // FS-UAE-verified exact placement.
    golden_gary_overlay => probe("gary-overlay", "gary-overlay.bin", 16.0);
    golden_fatgary_ramsey => Probe { name: "fatgary-ramsey", program: "fatgary-ramsey.bin", seconds: 8.0, machine: Machine::A4000 };
    // Gary's /VPA range and CIA selects by A12/A13, the register bank
    // repeating every $200, and the ROM mirrors, as eight green/blue bands.
    // The same program on the A1200 profile shows Gayle's decode.
    golden_gary_decode => probe("gary-decode", "gary-decode.bin", 16.0);
    golden_gayle_decode => Probe { name: "gayle-decode", program: "gary-decode.bin", seconds: 16.0, machine: Machine::A1200 };
    // Gary's CIA cycles keep CIA-A on D7-D0 and CIA-B on D15-D8, run a
    // long access as two word cycles, and run the E-clock cycle where
    // neither chip is selected, as five green bands.
    golden_gary_lanes => probe("gary-lanes", "gary-lanes.bin", 16.0);
    // Ramsey's DRAM cycle length in standard, page-hit, page-miss and skip
    // modes, as band heights.
    golden_ramsey_cycles => Probe { name: "ramsey-cycles", program: "ramsey-cycles.bin", seconds: 10.0, machine: Machine::A4000 };
    // Fat Gary's jumper-set ROM cycle against a five-clock Ramsey cycle: six
    // clocks on the A4000, eight on the 25 MHz A3000, one cycle a longword.
    golden_fatgary_romcycle => Probe { name: "fatgary-romcycle", program: "fatgary-romcycle.bin", seconds: 10.0, machine: Machine::A4000 };
    golden_fatgary_romcycle_a3000 => Probe { name: "fatgary-romcycle-a3000", program: "fatgary-romcycle.bin", seconds: 16.0, machine: Machine::A3000 };
    // The registers are a 16-bit CPU port beside Alice's 32-bit chip RAM
    // port: a long register access takes two cycles, a long RAM access one.
    golden_busprobe_reglong => probe_aga("busprobe-reglong", "busprobe-reglong.bin", 16.0);
    golden_lisa_scanblank => probe_aga("lisa-scanblank", "lisa-scanblank.bin", 16.0);
    // Lisa's fine DIWHIGH bits move a window edge in 70 ns (H1) and 35 ns
    // (H0) steps without moving the picture; run_probe checks each step.
    golden_diwhigh_fine => probe_aga("diwhigh-fine", "diwhigh-fine.bin", 16.0);
    // Lisa decodes six-plane hold-and-modify in super-hires, where OCS and
    // ECS Denise have only two planes to decode.
    golden_hamprobe_shres6 => probe_aga("hamprobe-shres6", "hamprobe-shres6.bin", 16.0);
    // ECS Denise blanks from Agnus's CSY* pin (HBSTRT..HBSTOP under
    // BLANKEN) only with BPLCON3.EXTBLKEN and BPLCON0.ECSENA, and latches
    // EXTBLKEN before ECSENA is set; an OCS Denise beside the same Agnus
    // keeps its fixed blank.
    golden_ecsblank_pin => Probe { name: "ecsblank-pin", program: "ecsblank-pin.bin", seconds: 16.0, machine: Machine::EcsDenise };
    golden_ecsblank_pin_ocs_denise => probe_ecs("ecsblank-pin-ocs-denise", "ecsblank-pin.bin", 16.0);
    // BPLCON2.KILLEHB is a bit of ECS Denise and Lisa: an OCS Denise
    // ignores it and keeps six planes extra-half-brite.
    golden_ehbprobe_kill => probe("ehbprobe-kill", "ehbprobe-kill.bin", 16.0);
    golden_ehbprobe_kill_ecs_denise => Probe { name: "ehbprobe-kill-ecs-denise", program: "ehbprobe-kill.bin", seconds: 16.0, machine: Machine::EcsDenise };
    // ECS Denise pairs super-hires pixels across each lo-res-wide group
    // into one colour register and splits its nibbles over the pair; an
    // OCS Denise beside the same Agnus keeps the per-pixel lookup.
    golden_shresprobe_pair => Probe { name: "shresprobe-pair", program: "shresprobe-pair.bin", seconds: 16.0, machine: Machine::EcsDenise };
    golden_shresprobe_pair_ocs_denise => probe_ecs("shresprobe-pair-ocs-denise", "shresprobe-pair.bin", 16.0);
    golden_dblpal_hires_lace =>probe_aga("dblpal-hires-lace", "dblpal-hires-lace.bin", 16.0);
    // Alice wide-fetch addressing: FMODE 10 duplicates the first word and
    // supplied low address bits alias phases inside 32-bit fetches.
    golden_aga_vamigats_fetch => probe_aga("agafetch-mode", "agafetch-mode.bin", 16.0);
    // Alice's valid bitplane counts depend on resolution and FMODE bandwidth;
    // overprogrammed counts fetch nothing rather than clamping.
    golden_aga_vamigats_planes => probe_aga("agaplanes", "agaplanes.bin", 16.0);
    // BPLCON3 SPRES 00/01/10/11 produces a 4:4:2:1 sprite-width staircase;
    // the final band is true 35 ns output, not another 70 ns HIRES band.
    golden_aga_shres_sprites => probe_aga("agashres-sprites", "agashres-sprites.bin", 16.0);
    // Lisa's eight-plane dual playfield: planes 7/8 extend each field's
    // colour index to four bits (the Zool AGA decode regression class),
    // PF2 reads the banked palette through the BPLCON3 PF2OF offset, and
    // sprite priority follows the same winning-field rule as OCS across
    // the sprprobe-dpfpri band table (in-range codes only; Lisa's
    // out-of-range colour behaviour deliberately diverges from vAmiga).
    // vAmiga-5-verified exact (bijective, 0 differing pixels).
    golden_dpfprobe_aga => probe_aga("dpfprobe-aga", "dpfprobe-aga.bin", 16.0);
    golden_dpfprobe_bplam => probe_aga("dpfprobe-bplam", "dpfprobe-bplam.bin", 16.0);
    golden_hamprobe_aga6 => probe_aga("hamprobe-aga6", "hamprobe-aga6.bin", 16.0);
    golden_bltprobe_final_d => Probe { name: "bltprobe-final-d", program: "bltprobe-final-d.bin", seconds: 16.0, machine: Machine::A1200 };
    golden_serprobe_zero => probe("serprobe-zero", "serprobe-zero.bin", 16.0);
    // HAM8: the control bits are the two LOWEST planes, a modify replaces
    // the top six bits of a component and holds the low two from the
    // previous pixel, and every non-set pixel depends on the line's full
    // history (the PR #563 indexed-cache regression class). Set-op sweep
    // through both palette banks, R/G/B modify ramps, a history weave,
    // and a LOCT-seeded low-bit-hold band. vAmiga-5-verified byte-exact
    // under its identity RGB monitor palette (VAMIGA_RGB=1).
    golden_hamprobe_ham8 => probe_aga("hamprobe-ham8", "hamprobe-ham8.bin", 16.0);
    // Lisa palette readback follows BPLCON3 BANK/LOCT and makes COLORxx
    // read-only while BPLCON2.RDRAM is set.
    golden_aga_vamigats_rdram => probe_aga("rdram-aga", "rdram-aga.bin", 16.0);
    // Lisa lands COLORxx changes one hires pixel after OCS/ECS Denise.
    golden_aga_vamigats_colorlag => probe_aga("colorlag-aga", "colorlag-aga.bin", 16.0);
    // BPLCON4 sprite palette banks: even sprites read the ESPRM high
    // nibble, odd sprites and attached pairs the OSPRM low nibble (the
    // Nexus 7 lamp-base regression class, where the swapped decode put an
    // even sprite on unset odd-bank entries). Colours vAmiga-verified
    // bijectively exact per band.
    golden_sprprobe_banks => probe_aga("sprprobe-banks", "sprprobe-banks.bin", 16.0);
    // Lisa's sub-pixel steps: BPLCON1 H1/H0 delay a lo-res playfield by
    // 70 and 35 ns, and SPRxCTL bits 4 and 3 move a sprite by the same in
    // every display mode. No reference emulator models either (see the
    // probe header).
    golden_agafine_scroll => probe_aga("agafine-scroll", "agafine-scroll.bin", 16.0);
    // Copper per-band slot budget across the line-end refresh access: the
    // $E2 refresh cycle stalls the CPU/blitter but must not block a Copper
    // fetch, or a saturated per-band MOVE stream loses one fetch per line
    // and its beam WAITs stop re-locking (the Nexus 7 plasma-zoom streak
    // regression class). vAmiga is not a valid reference here (see the
    // probe header).
    golden_copprobe_eol => probe_aga("copprobe-eol", "copprobe-eol.bin", 16.0);
    // CPU pacing bars under BLTPRI copy/fill/line blits (the Rampage
    // "present" flicker / BLS fence regression class).
    golden_bltprobe_pace => probe("bltprobe-pace", "bltprobe-pace.bin", 16.0);
    // 68020 chip writes under an all-channel BLTPRI-clear blit: a posted
    // write gets the blitter's three-miss yield (the write loop must not
    // stall for the whole blit).
    golden_bltprobe_post => probe_aga("bltprobe-post", "bltprobe-post.bin", 16.0);
    // VPOSR's V8 steps with VHPOSR's line byte: a read of both never pairs
    // the low byte of one line with the V8 of another (the false line 0 at
    // 255 -> 256 and line 256 at the frame wrap).
    golden_posprobe_v8 => probe("posprobe-v8", "posprobe-v8.bin", 16.0);
    // A 68000 long read of a custom register pair returns its first word
    // from that word's own bus cycle, two colour clocks before the second.
    golden_posprobe_long => probe("posprobe-long", "posprobe-long.bin", 16.0);
    // DMA sprite vertical reuse + attached pair placement (the sprite
    // register-FSM regression class).
    golden_ddfprobe_sprmulti => probe("ddfprobe-sprmulti", "ddfprobe-sprmulti.bin", 16.0);
    // CPU byte writes to a custom register latch the mirrored word (the
    // COLOR00 byte-write regression class).
    golden_regprobe_bytemirror => probe("regprobe-bytemirror", "regprobe-bytemirror.bin", 16.0);
    // A 68000 long write to a custom register pair lands each word on its
    // own bus cycle, two colour clocks apart, whichever instruction makes
    // it, and low word first to -(An); run_probe checks the edges against
    // a word write.
    golden_regprobe_longwrite => probe("regprobe-longwrite", "regprobe-longwrite.bin", 16.0);
    // The same on a 68010. The band from memory to an absolute address
    // also holds the Class 2 MOVE order (the write before the second
    // extension-word prefetch), the predecrement band its prefetch before
    // the write, and the delay loop the 68010's own DBcc times.
    golden_regprobe_longwrite_68010 => Probe { name: "regprobe-longwrite-68010", program: "regprobe-longwrite.bin", seconds: 16.0, machine: Machine::Ocs68010 };
    // A request unmasked in INTENA for two instructions still reaches the
    // CPU, on the 68000 and on the 68EC020 (the wait-loop class that keeps
    // a request pending and opens a window for its handler).
    golden_irqprobe_window => probe("irqprobe-window", "irqprobe-window.bin", 16.0);
    golden_irqprobe_window_aga => probe_aga("irqprobe-window-aga", "irqprobe-window.bin", 16.0);
    // The first DRAM bank repeats through the CPU's chip window on a
    // 512K-chip machine, on OCS and on an 8372A strapped for trapdoor RAM
    // alike (the class of loaders that size chip RAM by its alias).
    golden_memprobe_chipmirror => probe("memprobe-chipmirror", "memprobe-chipmirror.bin", 16.0);
    golden_memprobe_chipmirror_ecs => probe_ecs("memprobe-chipmirror-ecs", "memprobe-chipmirror.bin", 16.0);
    // CLXDAT collision matrix bits rendered as cells (the collision
    // matching/enable regression class).
    golden_clxprobe => probe("clxprobe", "clxprobe.bin", 16.0);
    // Raw startup/BBUSY/BZERO and descending destination-restore snapshots.
    golden_a500_blit_boundaries => probe("a500-blit-boundaries", "a500-blit-boundaries.bin", 16.0);
    golden_a500_display_boundaries => probe_ecs("a500-display-boundaries", "a500-display-boundaries.bin", 16.0);
    // Both CIA timer-A outputs, raw INTREQR, PRB and ICR across instruction
    // gaps. Model regression snapshot; electrical evidence and phase limits
    // are documented separately in internals/timing.md.
    golden_a500_cia_request => probe_ecs("a500-cia-request", "a500-cia-request.bin", 16.0);
    // AUD0 interrupt cadence strip across a scripted AUDxEN
    // enable/punch/disable/restart sequence (the issue #74 deferred
    // AUDxEN-disable regression class).
    golden_audprobe_en => probe("audprobe-en", "audprobe-en.bin", 16.0);
    // Sprite DMA fetches land in the Denise display latches: the terminator
    // CTL fetch disarms, DATA/DATB fetches overwrite, and a later bare
    // SPRxDATA arm redisplays the DMA-written words (the Hamazing
    // scene-switch stale-bar regression class).
    golden_sprprobe_latch => probe("sprprobe-latch", "sprprobe-latch.bin", 16.0);
    // An armed sprite has no vertical window in Denise: with sprite DMA off
    // it is serialized on every line whatever VSTART/VSTOP its control
    // words hold (empty, sixteen lines, inverted).
    golden_sprprobe_armline => probe("sprprobe-armline", "sprprobe-armline.bin", 16.0);
    // A SPRxCTL write between a DMA fetch slot and that channel's HSTART
    // disarms Denise before the serializer loads, cancelling the fetched
    // line; a write past HSTART cannot recall it (the Hybris panel
    // stray-dash regression class, issue #278). vAmiga-verified.
    golden_sprprobe_disarm => probe("sprprobe-disarm", "sprprobe-disarm.bin", 16.0);
    // The vertical display window is a set/reset flop: DIWSTOP rewrites
    // behind a closed flop never re-open it, a set/reset tie resets, and
    // an unreachable DIWSTOP holds the window open only to the frame's
    // last line, 312 (PAL), which forces a reset (the Kang Fu
    // CD32 screen-split regression class; the carried-across-vblank model
    // this probe caught lit the whole top of the frame).
    // vAmiga-verified byte-for-byte.
    golden_vdiwprobe_flop => probe("vdiwprobe-flop", "vdiwprobe-flop.bin", 16.0);
    // Under LACE the frame's last line that forces the vertical flop's
    // reset follows the field: an interlaced short field resets on its own
    // last line (311), so a window stopping on the long field's last line
    // (312) opens at DIWSTRT in both fields. A reset on the fixed line 312
    // alone carried the short field's window into the long field's top
    // (red rows here) and wove the Video Toaster 4.2 switcher's screen with
    // its fields swapped. vAmiga is not a valid reference (it models the
    // fixed line); see the probe header.
    golden_vdiwprobe_lace => probe("vdiwprobe-lace", "vdiwprobe-lace.bin", 16.0);
    // A whole-frame empty window (DIWSTRT.V == DIWSTOP.V parked below the
    // visible area via DIWHIGH) never opens and fetches nothing, so every
    // row rides the synthesized no-capture render path; the level window
    // test read the tie as a full wrap-around window and lit the frame
    // with undisplayed buffer contents (the Nexus 7 scene-transition
    // corrupted-ball regression class). BRDRSPRT sprites stay visible
    // over the closed frame.
    golden_vdiwprobe_empty => probe_aga("vdiwprobe-empty", "vdiwprobe-empty.bin", 16.0);
    // A DIWSTRT.V == DIWSTOP.V tie armed mid-frame over an ALREADY-OPEN
    // vertical flop changes nothing until the shared comparator line
    // (where reset wins): the rows between the rewrite and that line keep
    // fetching and displaying, so the renderer must take the row's DMA
    // capture as the vertical authority and read the tie as closed only
    // where nothing was fetched.
    golden_vdiwprobe_tieopen => probe("vdiwprobe-tieopen", "vdiwprobe-tieopen.bin", 16.0);
    // A bitplane fetch whose last unit overruns the line end (hires DDFSTRT
    // $3C with DDFSTOP $DC, one unit past the hard stop) is carried into
    // the next line, and the vertical flop's reset on DIWSTOP.V must drop
    // that carried run: the line after the last display line fetches
    // nothing. A per-row bar makes the per-line word accounting of the
    // carried unit visible; a standard $3C/$D4 band is the control.
    // Copperline kept the carried run going on the closed line and painted
    // the row from memory past the plane (the AROS boot-screen stray-line
    // regression class). vAmiga-verified byte-for-byte, drift included.
    golden_ddfprobe_vspill => probe("ddfprobe-vspill", "ddfprobe-vspill.bin", 16.0);
    // Dual-playfield sprite priority: a sprite pixel is tested against the
    // BPLCON2 code of the playfield that wins the PF1-vs-PF2 comparison
    // (opacity, then PF2PRI), so where both fields are opaque the losing
    // field's code is ignored -- including the circular programmings where
    // PF2PRI and the sprite codes disagree about the field order (the Chuck
    // Rock 2 player-in-front-of-the-bins regression class).
    // vAmiga-verified byte-for-byte over the whole frame.
    golden_sprprobe_dpfpri => probe("sprprobe-dpfpri", "sprprobe-dpfpri.bin", 16.0);
    // BPLCON0's HAM select reaches Denise in the colour-selection phase, so a
    // mid-line HAM change lands where a COLORxx write carried by the same
    // chip-bus slot would: eight bands clear HAM 16 colour clocks apart and
    // the blue/green staircase reads off the landing column (the Hollywood
    // Poker Pro HAM-photo/EHB-scoreboard split-line regression class).
    // vAmiga-verified: byte-identical over the whole frame.
    golden_hamprobe_select => probe("hamprobe-select", "hamprobe-select.bin", 16.0);
    // The HAM hold colour accumulates across the DIW left edge: with DDFSTRT
    // one fetch period before the window, the hidden border-masked samples
    // seed the hold colour the first visible pixel modifies. Two bands whose
    // visible fetch words are identical differ only in a hidden set-palette
    // pixel; both collapse to the same blue if the history is truncated at
    // the window edge (the Lemmings 2 FES demo DMA Design logo regression
    // class). vAmiga-verified band colours; the hidden span itself renders
    // as border per the vAmigaTS DIW-edge photos (see the probe header).
    golden_hamprobe_prediw => probe("hamprobe-prediw", "hamprobe-prediw.bin", 16.0);
    // Manual BPL1DAT writes (bitplane DMA off) load the serialiser on its
    // free-running word cadence, not at the write position: WAIT-sweep bars
    // snap to the word grid and DIW-clip to a straight edge, a re-arm before
    // the load strobe replaces the held word, and hires bars move per 4-cck
    // slot (the Hamazing Hexagon left-edge regression class).
    // vAmiga-verified: byte-identical over the whole frame.
    golden_bplprobe_dat => probe("bplprobe-dat", "bplprobe-dat.bin", 16.0);
    // The COPJMP strobes fire on a READ of the strobe address exactly as
    // on a write: a vblank handler's COP1LC rewrite plus TST.W COPJMP1
    // flips the running Copper list in the same frame (the Sleepwalker
    // CD32 double-buffer flip regression class). vAmiga-verified: the
    // stripe reads green (read strobed), not red (read inert).
    golden_copprobe_jmpread => probe("copprobe-jmpread", "copprobe-jmpread.bin", 16.0);
    // The Copper compares eight vertical bits: a WAIT for line $08 that
    // is evaluated on line 255 ($FF >= $08) releases at once, while the
    // same WAIT behind WAIT $FFDF is evaluated after the wrap and waits
    // for line 256+N. vAmiga-verified: red from line 255, green from
    // line $110.
    golden_copprobe_wait255 => probe("copprobe-wait255", "copprobe-wait255.bin", 16.0);
    // CLR.W COPJMP1 strobes on its read and on its write cycle. The Copper
    // requests the bus for a cycle before fetching, so the second strobe
    // restarts a list whose first MOVE rewrites COP1LC instead of jumping
    // through the rewritten value. vAmiga-verified: both stripes green.
    golden_copprobe_clrjmp => probe("copprobe-clrjmp", "copprobe-clrjmp.bin", 16.0);
}
