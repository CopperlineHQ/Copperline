use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

#[allow(dead_code)]
#[path = "../src/envcfg.rs"]
mod envcfg;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Debug, PartialEq)]
struct VAmigaTsCase {
    name: String,
    /// Output-relative path (directory of the case + the retrosh stem), so
    /// multiple shipped scripts per ADF (OCS/ECS/68010 variants) get
    /// distinct artifacts.
    rel_path: PathBuf,
    adf_path: PathBuf,
    /// vAmiga regression setup from the shipped .retrosh (machine model);
    /// mirrored onto the Copperline config's chipset revision.
    setup: String,
    /// CPU revision from the script's `cpu set revision` line (otherwise the
    /// selected machine setup's default); mirrored onto both emulators.
    cpu: String,
    /// Sum of the shipped script's `wait N seconds` lines
    /// (COPPERLINE_VAMIGATS_SECONDS still overrides globally when set).
    seconds: Option<f32>,
    /// Everything else the shipped script asks for: memory, video standard,
    /// extra drives and scripted input.
    script: ShippedScript,
    /// The vAmiga reference dump shipped beside the script (`<stem>.raw`).
    shipped_raw: Option<PathBuf>,
}

/// What a shipped vAmigaTS RetroShell regression script asks for.
#[derive(Clone, Debug, Default, PartialEq)]
struct ShippedScript {
    setup: Option<String>,
    cpu: Option<String>,
    adf: Option<String>,
    /// `mem set SLOW_RAM <KB>` (the stack-frame tests want none).
    slow_ram_kb: Option<u32>,
    /// `amiga set VIDEO_FORMAT NTSC`.
    ntsc: bool,
    /// Wired floppy drives: the highest `dfN connect` index plus one.
    drives: u8,
    /// Machine-configuration lines a generated vAmiga reference script
    /// replays verbatim ahead of `regression run`.
    vamiga_config_lines: Vec<String>,
    /// `screenshot set cutout`: the part of the frame the test compares,
    /// as `[x1, y1, x2, y2]` in vAmiga texture coordinates (half-open).
    cutout: Option<[u32; 4]>,
    /// Waits and input in script order.
    steps: Vec<ScriptStep>,
}

#[derive(Clone, Debug, PartialEq)]
enum ScriptStep {
    Wait(f32),
    /// `joystickN pull <direction>`: the stick is held from here on.
    JoystickPull {
        port: u8,
        direction: String,
    },
    /// `mouseN press <button>`: a press and release.
    MousePress {
        port: u8,
        button: String,
    },
    /// `keyboard press <code>`: a raw Amiga key code, pressed and released.
    KeyPress {
        code: u8,
    },
}

impl ScriptStep {
    /// The step in the RetroShell syntax current vAmiga builds accept
    /// (`joystick1 ...`; scripts from older checkouts write `joystick 1 ...`,
    /// which vAmiga 4.4 and later reject).
    fn retroshell_line(&self) -> Option<String> {
        match self {
            ScriptStep::Wait(_) => None,
            ScriptStep::JoystickPull { port, direction } => {
                Some(format!("joystick{port} pull {direction}"))
            }
            ScriptStep::MousePress { port, button } => Some(format!("mouse{port} press {button}")),
            ScriptStep::KeyPress { code } => Some(format!("keyboard press {code}")),
        }
    }
}

impl ShippedScript {
    /// Total of the script's waits: when its screenshot is taken.
    fn total_seconds(&self) -> Option<f32> {
        let mut total = None;
        for step in &self.steps {
            if let ScriptStep::Wait(secs) = step {
                total = Some(total.unwrap_or(0.0) + secs);
            }
        }
        total
    }

    /// Input steps with the emulated time each one happens at.
    fn timed_inputs(&self) -> Vec<(f32, &ScriptStep)> {
        let mut now = 0.0;
        let mut inputs = Vec::new();
        for step in &self.steps {
            match step {
                ScriptStep::Wait(secs) => now += secs,
                input => inputs.push((now, input)),
            }
        }
        inputs
    }
}

/// Where each case's vAmiga reference dump comes from.
#[derive(Debug)]
enum ReferenceSource {
    /// No reference: screenshots only.
    None,
    /// The `<stem>.raw` dump vAmigaTS ships beside each script.
    Shipped,
    /// A local `VAHeadless` run of a generated script.
    VAmiga(VAmigaReference),
}

#[derive(Debug)]
struct VAmigaReference {
    executable: PathBuf,
    /// COPPERLINE_VAMIGATS_VAMIGA_SETUP override; when unset each case runs
    /// on the machine its shipped script names.
    setup_override: Option<String>,
}

#[test]
#[ignore = "requires COPPERLINE_VAMIGATS_DIR plus a local Kickstart 1.3 ROM"]
fn run_vamiga_ts_adf_screenshots() -> TestResult {
    let Some((root, kick13)) = suite_root_and_kickstart()? else {
        return Ok(());
    };

    let mut cases = discover_adf_cases(&root)?;
    let filter = envcfg::var("COPPERLINE_VAMIGATS_FILTER");
    if let Some(filter) = filter.as_deref() {
        cases.retain(|case| case.name.contains(filter));
    }
    if let Some(limit) = parse_optional_usize("COPPERLINE_VAMIGATS_LIMIT")? {
        cases.truncate(limit);
    }
    if cases.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no vAmigaTS .adf tests selected under {}{}",
                root.display(),
                filter
                    .as_deref()
                    .map(|f| format!(" with filter {f:?}"))
                    .unwrap_or_default()
            ),
        )
        .into());
    }

    // A global COPPERLINE_VAMIGATS_SECONDS overrides everything; otherwise
    // each case is captured when its shipped script takes its screenshot
    // (the sum of the script's waits, default 9s).
    let seconds_override = envcfg::var("COPPERLINE_VAMIGATS_SECONDS")
        .map(|s| s.parse::<f32>())
        .transpose()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let out_root = output_root()?;
    let baseline_root = env_path("COPPERLINE_VAMIGATS_BASELINE");
    let reference = reference_source()?;
    let jobs = job_count()?;

    eprintln!(
        "running {} vAmigaTS case(s) from {} on {jobs} job(s); output {}",
        cases.len(),
        root.display(),
        out_root.display()
    );
    let emulator = env!("CARGO_BIN_EXE_copperline");
    run_parallel(&cases, jobs, |case| {
        let seconds = seconds_override.or(case.seconds).unwrap_or(9.0);
        run_case(
            emulator,
            &kick13,
            &out_root,
            baseline_root.as_deref(),
            &reference,
            seconds,
            case,
        )
    })
}

/// The self-checking CPU tests ship no RetroShell script and no reference
/// picture: each compares what it measured (an exception stack frame, the
/// registers after a fault, a trace sequence) against the values recorded on
/// real machines and paints the verdict as the background colour. Each runs
/// here on every CPU it carries expected values for.
#[test]
#[ignore = "requires COPPERLINE_VAMIGATS_DIR plus a local Kickstart 1.3 ROM"]
fn run_vamiga_ts_cpu_self_checks() -> TestResult {
    let Some((root, kick13)) = suite_root_and_kickstart()? else {
        return Ok(());
    };

    let mut cases = discover_self_check_cases(&root)?;
    if let Some(filter) = envcfg::var("COPPERLINE_VAMIGATS_FILTER") {
        cases.retain(|case| case.name.contains(&filter));
    }
    if cases.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no self-checking vAmigaTS tests selected under {}",
                root.display()
            ),
        )
        .into());
    }
    let out_root = output_root()?;
    let jobs = job_count()?;
    let runs = cases
        .iter()
        .flat_map(|case| {
            SELF_CHECK_MACHINES
                .iter()
                .filter(|machine| case.cpus.contains(&machine.cpu))
                .map(move |machine| (case, *machine))
        })
        .collect::<Vec<_>>();

    eprintln!(
        "running {} self-checking vAmigaTS test(s), {} run(s) on {jobs} job(s); output {}",
        cases.len(),
        runs.len(),
        out_root.display()
    );
    let emulator = env!("CARGO_BIN_EXE_copperline");
    let verdicts = Mutex::new(Vec::new());
    run_parallel(&runs, jobs, |(case, machine)| {
        let verdict = run_self_check(emulator, &kick13, &out_root, case, *machine)?;
        verdicts
            .lock()
            .unwrap()
            .push((case.name.clone(), machine.cpu, verdict));
        Ok(())
    })?;

    let mut verdicts = verdicts.into_inner().unwrap();
    verdicts.sort();
    let report = verdicts
        .iter()
        .map(|(name, cpu, verdict)| format!("{name}\t{cpu}\t{verdict}\n"))
        .collect::<String>();
    let report_path = out_root.join("selfcheck.tsv");
    fs::write(&report_path, &report)?;
    for machine in SELF_CHECK_MACHINES {
        let of_cpu = verdicts.iter().filter(|(_, cpu, _)| *cpu == machine.cpu);
        let pass = of_cpu.clone().filter(|(_, _, v)| v == "pass").count();
        let fail = of_cpu.clone().filter(|(_, _, v)| v == "fail").count();
        eprintln!(
            "{}: {pass} pass, {fail} fail, {} other",
            machine.cpu,
            of_cpu.count() - pass - fail
        );
    }
    eprintln!("verdicts written to {}", report_path.display());

    if let Some(baseline) = env_path("COPPERLINE_VAMIGATS_SELFCHECK_BASELINE") {
        let regressed = self_check_regressions(&fs::read_to_string(&baseline)?, &report);
        if !regressed.is_empty() {
            return Err(io::Error::other(format!(
                "{} self-check(s) passed in {} and no longer do:\n{}",
                regressed.len(),
                baseline.display(),
                regressed.join("\n")
            ))
            .into());
        }
    }
    Ok(())
}

/// Resolve the suite checkout and the Kickstart 1.3 ROM, or explain why the
/// run is skipped.
fn suite_root_and_kickstart() -> TestResult<Option<(PathBuf, PathBuf)>> {
    let Some(root) = env_path("COPPERLINE_VAMIGATS_DIR") else {
        eprintln!("skipping vAmigaTS run; set COPPERLINE_VAMIGATS_DIR to a vAmigaTS checkout");
        return Ok(None);
    };
    let root = root.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "canonicalizing COPPERLINE_VAMIGATS_DIR {}: {e}",
                root.display()
            ),
        )
    })?;
    let Some(kick13) = kickstart_13_path() else {
        eprintln!(
            "skipping vAmigaTS run; set COPPERLINE_VAMIGATS_KICK13 or provide {} or /tmp/kick13.rom",
            repo_root().join("KICK13.ROM").display()
        );
        return Ok(None);
    };
    let kick13 = kick13.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("canonicalizing Kickstart 1.3 ROM {}: {e}", kick13.display()),
        )
    })?;
    Ok(Some((root, kick13)))
}

fn output_root() -> TestResult<PathBuf> {
    let out_root = env_path("COPPERLINE_VAMIGATS_OUT")
        .unwrap_or_else(|| unique_temp_dir("copperline-vamigats"));
    fs::create_dir_all(&out_root)?;
    Ok(out_root.canonicalize()?)
}

fn job_count() -> TestResult<usize> {
    Ok(parse_optional_usize("COPPERLINE_VAMIGATS_JOBS")?
        .unwrap_or(1)
        .max(1))
}

fn reference_source() -> TestResult<ReferenceSource> {
    let shipped = envcfg::flag("COPPERLINE_VAMIGATS_SHIPPED_REFS");
    let vamiga = env_path("COPPERLINE_VAMIGATS_VAMIGA");
    match (shipped, vamiga) {
        (true, Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "set COPPERLINE_VAMIGATS_SHIPPED_REFS or COPPERLINE_VAMIGATS_VAMIGA, not both",
        )
        .into()),
        (true, None) => Ok(ReferenceSource::Shipped),
        (false, Some(executable)) => Ok(ReferenceSource::VAmiga(VAmigaReference {
            executable,
            setup_override: envcfg::var("COPPERLINE_VAMIGATS_VAMIGA_SETUP"),
        })),
        (false, None) => Ok(ReferenceSource::None),
    }
}

/// Run `run` over `items` on `jobs` threads. The emulator is deterministic
/// and every case owns its output files, so cases are independent; the first
/// failure stops the remaining work and is returned.
fn run_parallel<T: Sync>(
    items: &[T],
    jobs: usize,
    run: impl Fn(&T) -> TestResult + Sync,
) -> TestResult {
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, items.len().max(1)) {
            scope.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    if let Err(e) = run(item) {
                        failed.store(true, Ordering::Relaxed);
                        first_error.lock().unwrap().get_or_insert(e.to_string());
                    }
                }
            });
        }
    });
    match first_error.into_inner().unwrap() {
        Some(message) => Err(io::Error::other(message).into()),
        None => Ok(()),
    }
}

fn run_case(
    emulator: &str,
    kick13: &Path,
    out_root: &Path,
    baseline_root: Option<&Path>,
    reference: &ReferenceSource,
    seconds: f32,
    case: &VAmigaTsCase,
) -> TestResult {
    let png_path = run_copperline(
        emulator,
        kick13,
        &out_root.join(&case.rel_path),
        &case.name,
        &case.adf_path,
        &case.setup,
        &case.cpu,
        &case.script,
        seconds,
    )?;

    // A screenshot of any other size means the presentation path changed
    // shape: 716x537 is the 4:3 glass (PRESENT_HEIGHT_TV = FB_WIDTH * 3/4,
    // the woven scanlines resampled onto the TV-aspect canvas), and
    // COPPERLINE_SHOT_RAW saves the woven native framebuffer instead
    // (716x570 = the vAmiga regression cutout with line doubling, the
    // format tools/vamigats-compare.py consumes -- sweeps set it).
    if envcfg::flag("COPPERLINE_SHOT_RAW") {
        assert_png_dimensions(&png_path, 716, 570)?;
    } else {
        assert_png_dimensions(&png_path, 716, 537)?;
    }
    if let Some(baseline_root) = baseline_root {
        let mut expected = baseline_root.join(&case.rel_path);
        expected.set_extension("png");
        compare_png_bytes(&expected, &png_path, &case.name)?;
    }
    match reference {
        ReferenceSource::None => {}
        ReferenceSource::Shipped => copy_shipped_reference(out_root, case)?,
        ReferenceSource::VAmiga(vamiga) => {
            run_vamiga_reference(vamiga, kick13, out_root, seconds, case)?
        }
    }
    Ok(())
}

/// Boot `adf` on the machine a script describes and save the frame at
/// `seconds` as `<out_base>.png`. `out_base` names the case's output files;
/// its extension is replaced for the config, the scratch disk and the
/// screenshot.
fn run_copperline(
    emulator: &str,
    kick13: &Path,
    out_base: &Path,
    name: &str,
    adf: &Path,
    setup: &str,
    cpu: &str,
    script: &ShippedScript,
    seconds: f32,
) -> TestResult<PathBuf> {
    let cfg_path = out_base.with_extension("toml");
    let png_path = out_base.with_extension("png");
    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Run against a scratch copy of the ADF, mounted writable: vAmiga's
    // regression flow copies the disk to /tmp and inserts it unprotected,
    // and several Drive/step and Drive/read cases visualize the CIA-A
    // /WPRO line, so a write-protected mount diverges from the reference
    // (and from the shipped real-hardware photos).
    let disk_path = out_base.with_extension("df0.adf");
    fs::copy(adf, &disk_path)?;
    fs::write(
        &cfg_path,
        copperline_config(kick13, &disk_path, setup, cpu, script),
    )?;

    let output = Command::new(emulator)
        .env("COPPERLINE_HCENTER", "0")
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn")
        .arg("--noaudio")
        .arg("--config")
        .arg(&cfg_path)
        .args(copperline_input_args(script))
        .arg("--screenshot-after")
        .arg(format!("{seconds:.3}"))
        .arg(&png_path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{name} exited with {}\nstdout tail:\n{}\nstderr tail:\n{}",
            output.status,
            tail_text(&output.stdout),
            tail_text(&output.stderr)
        ))
        .into());
    }
    Ok(png_path)
}

/// How long a `joystickN pull` holds the stick: vAmiga keeps it pulled
/// until the script releases it, which no shipped script does.
const JOYSTICK_HOLD_MS: &str = "600000";

/// The shipped script's extra drives and scripted input as Copperline
/// command-line flags.
fn copperline_input_args(script: &ShippedScript) -> Vec<String> {
    let mut args = Vec::new();
    if script.drives > 1 {
        args.extend(["--floppy-drives".to_owned(), script.drives.to_string()]);
    }
    let mut stick_in_port_1 = false;
    for (at, step) in script.timed_inputs() {
        let at = format!("{at:.3}");
        match step {
            ScriptStep::Wait(_) => {}
            ScriptStep::JoystickPull { port, direction } => {
                stick_in_port_1 |= *port == 1;
                args.extend([
                    "--joy-after".to_owned(),
                    at,
                    direction.clone(),
                    JOYSTICK_HOLD_MS.to_owned(),
                    port.to_string(),
                ]);
            }
            ScriptStep::MousePress { port, button } => args.extend([
                "--click-after".to_owned(),
                at,
                button.clone(),
                "100".to_owned(),
                port.to_string(),
            ]),
            ScriptStep::KeyPress { code } => {
                args.extend(["--press-after".to_owned(), at, format!("{code:#04x}")]);
            }
        }
    }
    // Port 1 carries the mouse unless the script plugs a stick into it.
    if stick_in_port_1 {
        args.extend(["--port1".to_owned(), "joystick".to_owned()]);
    }
    args
}

/// Use the reference dump the suite ships beside the script. Scripts with
/// no dump (a handful are run by hand upstream) are left without one.
fn copy_shipped_reference(out_root: &Path, case: &VAmigaTsCase) -> TestResult {
    let Some(shipped) = case.shipped_raw.as_deref() else {
        return Ok(());
    };
    let raw = fs::read(shipped)?;
    if raw.len() != 716 * 285 * 3 {
        return Err(io::Error::other(format!(
            "{}: shipped reference {} has {} bytes, expected 716x285 RGB = {}",
            case.name,
            shipped.display(),
            raw.len(),
            716 * 285 * 3
        ))
        .into());
    }
    fs::write(
        out_root.join(&case.rel_path).with_extension("vamiga.raw"),
        raw,
    )?;
    Ok(())
}

fn run_vamiga_reference(
    reference: &VAmigaReference,
    kick13: &Path,
    out_root: &Path,
    seconds: f32,
    case: &VAmigaTsCase,
) -> TestResult {
    let stem = vamiga_temp_stem(case);
    let tmp_dir = std::env::temp_dir();
    let tmp_adf = tmp_dir.join(format!("{stem}.adf"));
    let tmp_kick = tmp_dir.join(format!("{stem}-kick13.rom"));
    // vAmiga's RegressionTester hardcodes /tmp for the texture dump
    // (macOS's env::temp_dir() points elsewhere).
    let tmp_raw = PathBuf::from("/tmp").join(format!("{stem}.raw"));
    let mut script_path = out_root.join(&case.rel_path);
    script_path.set_extension("vamiga.retrosh");
    let mut raw_path = out_root.join(&case.rel_path);
    raw_path.set_extension("vamiga.raw");

    let result: TestResult = (|| {
        fs::copy(&case.adf_path, &tmp_adf)?;
        fs::copy(kick13, &tmp_kick)?;
        if let Some(parent) = script_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &script_path,
            vamiga_retroshell_script(
                reference.setup_override.as_deref().unwrap_or(&case.setup),
                &case.cpu,
                &tmp_kick,
                &tmp_adf,
                seconds,
                &stem,
                &case.script,
            ),
        )?;

        let output = Command::new(&reference.executable)
            .arg(&script_path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "{} vAmiga reference exited with {}\nstdout tail:\n{}\nstderr tail:\n{}",
                case.name,
                output.status,
                tail_text(&output.stdout),
                tail_text(&output.stderr)
            ))
            .into());
        }

        let mut raw = fs::read(&tmp_raw).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{}: reading vAmiga raw output {}: {e}",
                    case.name,
                    tmp_raw.display()
                ),
            )
        })?;
        if raw.len() != 716 * 285 * 3 {
            return Err(io::Error::other(format!(
                "{}: vAmiga raw output has {} bytes, expected 716x285 RGB = {}",
                case.name,
                raw.len(),
                716 * 285 * 3
            ))
            .into());
        }
        // The generated script takes a full-frame screenshot; the shipped
        // script's cutout is applied to the dump instead.
        if let Some(cutout) = case.script.cutout {
            mask_vamiga_dump_outside_cutout(&mut raw, cutout);
        }
        fs::write(&raw_path, raw)?;
        Ok(())
    })();

    let _ = fs::remove_file(tmp_adf);
    let _ = fs::remove_file(tmp_kick);
    let _ = fs::remove_file(tmp_raw);
    result
}

fn copperline_config(
    kick13: &Path,
    adf: &Path,
    setup: &str,
    cpu: &str,
    script: &ShippedScript,
) -> String {
    let machine = machine_for_setup(setup);
    let slow = match script.slow_ram_kb {
        Some(0) => "0".to_owned(),
        Some(kb) => format!("{kb}K"),
        None => machine.slow.to_owned(),
    };
    let video = if script.ntsc { "NTSC" } else { "PAL" };
    format!(
        r#"rom = {}

[display]
# Full overscan without recentring: the comparator aligns raw beam
# coordinates, so the TV bezel mask and the content recentring shift
# must both stay out of the dump (COPPERLINE_HCENTER=0 is set on the
# spawned process for the same reason).
overscan = "full"

[emulation]
speed = "turbo"

[cpu]
model = "{cpu}"
fpu = false

[memory]
chip = "{}"
fast = "0"
slow = "{slow}"

[chipset]
{}
video = "{video}"

[floppy.df0]
path = {}
write_protected = false
"#,
        toml_string(&kick13.to_string_lossy()),
        machine.chip,
        machine.chipset,
        toml_string(&adf.to_string_lossy())
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VAmigaMachine {
    default_cpu: &'static str,
    chip: &'static str,
    slow: &'static str,
    chipset: &'static str,
}

fn machine_for_setup(setup: &str) -> VAmigaMachine {
    match setup {
        "A1200_2MB" => VAmigaMachine {
            default_cpu: "68EC020",
            chip: "2M",
            slow: "0",
            chipset: "revision = \"AGA\"\n",
        },
        // vAmiga's A500 ECS scheme pairs the 8372A Agnus with the original
        // OCS Denise; using the broad ECS preset would silently change Lisa's
        // side of many register/display tests.
        "A500_ECS_1MB" => VAmigaMachine {
            default_cpu: "68000",
            chip: "512K",
            slow: "512K",
            chipset: "revision = \"ECS\"\nagnus = \"8372A\"\ndenise = \"OCS\"\n",
        },
        "A500_PLUS_1MB" => VAmigaMachine {
            default_cpu: "68000",
            chip: "512K",
            slow: "512K",
            chipset: "revision = \"ECS\"\nagnus = \"8375\"\ndenise = \"ECS\"\n",
        },
        // A1000_OCS_1MB and A500_OCS_1MB differ in early Agnus details that
        // Copperline currently represents with the same OCS revision.
        _ => VAmigaMachine {
            default_cpu: "68000",
            chip: "512K",
            slow: "512K",
            chipset: "revision = \"OCS\"\n",
        },
    }
}

fn discover_adf_cases(root: &Path) -> TestResult<Vec<VAmigaTsCase>> {
    let mut cases = Vec::new();
    collect_adf_cases(root, root, &mut cases)?;
    cases.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(cases)
}

/// Parse a shipped vAmigaTS RetroShell regression script. Keywords are
/// matched without regard to case (the suite writes both `cpu set revision`
/// and `cpu set REVISION`), and both device spellings are accepted
/// (`joystick 1 pull left` in older checkouts, `joystick1 pull left` now).
fn parse_shipped_retrosh(text: &str) -> ShippedScript {
    let mut script = ShippedScript {
        drives: 1,
        ..ShippedScript::default()
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let words = line.split_whitespace().collect::<Vec<_>>();
        let lower = words
            .iter()
            .map(|word| word.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let lower = lower.iter().map(String::as_str).collect::<Vec<_>>();
        match lower.as_slice() {
            ["regression", "setup", ..] => script.setup = words.get(2).map(|s| (*s).to_owned()),
            ["regression", "run", ..] => {
                script.adf = words
                    .get(2)
                    .and_then(|p| Path::new(p).file_name())
                    .map(|n| n.to_string_lossy().into_owned());
            }
            ["cpu", "set", "revision", ..] => script.cpu = words.get(3).map(|s| (*s).to_owned()),
            ["wait", secs, ..] => {
                if let Ok(secs) = secs.parse() {
                    script.steps.push(ScriptStep::Wait(secs));
                }
            }
            ["mem", "set", "slow_ram", kb, ..] => {
                script.slow_ram_kb = kb.parse().ok();
                script.vamiga_config_lines.push(line.to_owned());
            }
            ["amiga", "set", "video_format", format, ..] => {
                script.ntsc = *format == "ntsc";
                script.vamiga_config_lines.push(line.to_owned());
            }
            [drive, "connect"] if floppy_drive_index(drive).is_some() => {
                let index = floppy_drive_index(drive).unwrap_or(0);
                script.drives = script.drives.max(index + 1);
                script.vamiga_config_lines.push(line.to_owned());
            }
            // Reset-time register values and the A1200 scheme's overclock
            // workaround have no Copperline counterpart, but a generated
            // reference script still carries them.
            ["cpu", "set", "reset_val", ..]
            | ["cpu", "set", "overclocking", ..]
            | ["amiga", "reset"] => script.vamiga_config_lines.push(line.to_owned()),
            ["screenshot", "set", "cutout", bounds @ ..] => {
                script.cutout = parse_cutout(bounds);
            }
            _ => {
                if let Some(step) = parse_input_step(&lower) {
                    script.steps.push(step);
                }
            }
        }
    }
    script
}

/// The four bounds of `screenshot set cutout`, which upstream writes as
/// `x1=300 y1=26 x2=912 y2=311` and older checkouts as `300 26 912 311`.
fn parse_cutout(bounds: &[&str]) -> Option<[u32; 4]> {
    let [x1, y1, x2, y2] = bounds else {
        return None;
    };
    let bound = |word: &str, name: &str| -> Option<u32> {
        let value = match word.split_once('=') {
            Some((key, value)) if key == name => value,
            Some(_) => return None,
            None => word,
        };
        value.parse().ok()
    };
    Some([
        bound(x1, "x1")?,
        bound(y1, "y1")?,
        bound(x2, "x2")?,
        bound(y2, "y2")?,
    ])
}

/// The top-left texel of a vAmiga regression dump (716x285 from here).
const VAMIGA_DUMP_X1: u32 = 196;
const VAMIGA_DUMP_Y1: u32 = 26;
const VAMIGA_DUMP_WIDTH: usize = 716;
const VAMIGA_DUMP_HEIGHT: usize = 285;

/// Blank a regression dump outside a script's cutout the way vAmiga's
/// RegressionTester does: an 8-texel grey checkerboard (0x22 / 0x44) laid
/// over the texture grid. The scoring tools recognise that fill and leave
/// it out, so a generated reference scores the same region as the shipped
/// one. Done here instead of in the generated script because the spelling
/// of `screenshot set cutout` differs between VAHeadless versions.
fn mask_vamiga_dump_outside_cutout(raw: &mut [u8], cutout: [u32; 4]) {
    let [x1, y1, x2, y2] = cutout;
    for (row, line) in raw
        .chunks_exact_mut(VAMIGA_DUMP_WIDTH * 3)
        .take(VAMIGA_DUMP_HEIGHT)
        .enumerate()
    {
        let y = VAMIGA_DUMP_Y1 + row as u32;
        for (col, texel) in line.chunks_exact_mut(3).enumerate() {
            let x = VAMIGA_DUMP_X1 + col as u32;
            if (y1..y2).contains(&y) && (x1..x2).contains(&x) {
                continue;
            }
            let grey = if (y >> 3) & 1 == (x >> 3) & 1 {
                0x22
            } else {
                0x44
            };
            texel.fill(grey);
        }
    }
}

/// `df1` .. `df3` as a drive index.
fn floppy_drive_index(word: &str) -> Option<u8> {
    let index = word.strip_prefix("df")?.parse::<u8>().ok()?;
    (index <= 3).then_some(index)
}

/// Split `joystick1 ...` or `joystick 1 ...` into the port number and the
/// remaining words.
fn device_port<'a>(words: &'a [&'a str], device: &str) -> Option<(u8, &'a [&'a str])> {
    let suffix = words.first()?.strip_prefix(device)?;
    if suffix.is_empty() {
        Some((words.get(1)?.parse().ok()?, &words[2..]))
    } else {
        Some((suffix.parse().ok()?, &words[1..]))
    }
}

fn parse_input_step(words: &[&str]) -> Option<ScriptStep> {
    if let Some((port, ["pull", direction])) = device_port(words, "joystick") {
        return Some(ScriptStep::JoystickPull {
            port,
            direction: (*direction).to_owned(),
        });
    }
    if let Some((port, ["press", button])) = device_port(words, "mouse") {
        return Some(ScriptStep::MousePress {
            port,
            button: (*button).to_owned(),
        });
    }
    if let ["keyboard", "press", code] = words {
        return Some(ScriptStep::KeyPress {
            code: code.parse().ok()?,
        });
    }
    None
}

fn collect_adf_cases(root: &Path, dir: &Path, cases: &mut Vec<VAmigaTsCase>) -> TestResult {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    let mut adfs_with_scripts: Vec<PathBuf> = Vec::new();
    let mut bare_adfs: Vec<PathBuf> = Vec::new();
    for entry in &entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            if entry.file_name() != ".git" {
                collect_adf_cases(root, &path, cases)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("adf") => bare_adfs.push(path),
            // Shipped scripts drive the case's machine/CPU/duration; skip the
            // harness's own generated reference scripts.
            Some("retrosh") if !path.to_string_lossy().ends_with(".vamiga.retrosh") => {
                let script = parse_shipped_retrosh(&fs::read_to_string(&path)?);
                let Some(adf_name) = script.adf.clone() else {
                    continue;
                };
                // The suite's Makefile copies the case's shipped ADF to
                // /tmp/<script-stem>.adf before running, so the script's ADF
                // name rarely matches the shipped file. Resolve to the named
                // file when present, otherwise to the directory's ADF.
                let named = dir.join(&adf_name);
                let adf_path = if named.exists() {
                    named
                } else {
                    let mut adfs = fs::read_dir(dir)?
                        .filter_map(|e| e.ok())
                        .map(|e| e.path())
                        .filter(|p| p.extension().and_then(|ext| ext.to_str()) == Some("adf"))
                        .collect::<Vec<_>>();
                    adfs.sort();
                    match adfs.into_iter().next() {
                        Some(p) => p,
                        None => continue,
                    }
                };
                let rel_path = path.strip_prefix(root)?.with_extension("");
                let name = rel_path
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<String>>()
                    .join("/");
                adfs_with_scripts.push(adf_path.clone());
                let setup = script
                    .setup
                    .clone()
                    .unwrap_or_else(|| "A500_OCS_1MB".to_owned());
                let cpu = script
                    .cpu
                    .clone()
                    .unwrap_or_else(|| machine_for_setup(&setup).default_cpu.to_owned());
                cases.push(VAmigaTsCase {
                    name,
                    rel_path: rel_path.with_extension("adf"),
                    adf_path,
                    setup,
                    cpu,
                    seconds: script.total_seconds(),
                    script,
                    shipped_raw: existing_path(path.with_extension("raw")),
                });
            }
            _ => {}
        }
    }
    // ADFs without any shipped script keep the old default-machine behaviour.
    for path in bare_adfs {
        if adfs_with_scripts.contains(&path) {
            continue;
        }
        let rel_path = path.strip_prefix(root)?.to_path_buf();
        let name = rel_path
            .with_extension("")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<String>>()
            .join("/");
        cases.push(VAmigaTsCase {
            name,
            rel_path,
            adf_path: path,
            setup: "A500_OCS_1MB".to_owned(),
            cpu: "68000".to_owned(),
            seconds: None,
            script: ShippedScript::default(),
            shipped_raw: None,
        });
    }
    Ok(())
}

fn vamiga_retroshell_script(
    setup: &str,
    cpu: &str,
    kick13: &Path,
    adf: &Path,
    seconds: f32,
    screenshot_stem: &str,
    script: &ShippedScript,
) -> String {
    let mut out = String::from("# Regression reference script generated by Copperline\n");
    out.push_str(&format!("regression setup {setup} {}\n", kick13.display()));
    // vAmiga 4.4's RetroShell registers option setters under the option's
    // uppercase key: `cpu set REVISION 68010` (the suite's older scripts use
    // a lowercase spelling it rejects).
    if cpu != machine_for_setup(setup).default_cpu {
        out.push_str(&format!("cpu set REVISION {cpu}\n"));
    }
    // vAmiga leaves collision detection off unless a script asks for it;
    // Denise always detects, and CLXDAT tests are meaningless without it.
    for kind in ["SPR_SPR", "SPR_PLF", "PLF_PLF"] {
        out.push_str(&format!("denise set CLX_{kind} true\n"));
    }
    for line in &script.vamiga_config_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&format!("regression run {}\n", adf.display()));
    // Replay the script's input at its own times; RetroShell's wait takes an
    // integer second count.
    let mut now = 0.0;
    let mut wait_until = |out: &mut String, at: f32| {
        if at > now {
            out.push_str(&format!("wait {} seconds\n", (at - now).ceil() as u32));
            now = at;
        }
    };
    for (at, step) in script.timed_inputs() {
        if at >= seconds {
            break;
        }
        wait_until(&mut out, at);
        if let Some(line) = step.retroshell_line() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    wait_until(&mut out, seconds);
    out.push_str(&format!("screenshot save {screenshot_stem}\n"));
    out
}

/// A vAmigaTS test that judges itself: no shipped script, and a source that
/// pulls in one of the suite's result harnesses and carries the values to
/// expect.
#[derive(Clone, Debug, PartialEq)]
struct SelfCheckCase {
    name: String,
    rel_path: PathBuf,
    adf_path: PathBuf,
    /// The CPUs the source holds expected values for.
    cpus: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug)]
struct SelfCheckMachine {
    cpu: &'static str,
    setup: &'static str,
    /// The label of this CPU's expected values in a test's source.
    expected_label: &'static str,
}

/// The CPUs a harness picks expected values for at run time (from exec's
/// AttnFlags). A test runs only where its source defines that CPU's
/// array, which keeps the 68020-only tests off the 68000 and leaves out
/// the 68030 MMU tests, whose references need a different machine. The
/// 68000 and 68010 runs use the configuration the suite documents for
/// these tests (512K chip RAM and nothing else); the 68EC020 runs on the
/// A1200 scheme.
const SELF_CHECK_MACHINES: [SelfCheckMachine; 3] = [
    SelfCheckMachine {
        cpu: "68000",
        setup: "A500_ECS_1MB",
        expected_label: "expected000:",
    },
    SelfCheckMachine {
        cpu: "68010",
        setup: "A500_ECS_1MB",
        expected_label: "expected010:",
    },
    SelfCheckMachine {
        cpu: "68EC020",
        setup: "A1200_2MB",
        expected_label: "expected020:",
    },
];

/// The background colours the result harnesses paint (`COLOR00`), as the
/// 8-bit RGB a screenshot holds.
const SELF_CHECK_VERDICTS: [([u8; 3], &str); 4] = [
    // $060: every measured value matched the expectation.
    ([0x00, 0x66, 0x00], "pass"),
    // $600: at least one value differed (the differing rows are drawn in
    // the second bitplane's colour).
    ([0x66, 0x00, 0x00], "fail"),
    // $0F0: the instruction under test raised no exception.
    ([0x00, 0xFF, 0x00], "no-exception"),
    // $F00: an exception other than the expected one.
    ([0xFF, 0x00, 0x00], "wrong-exception"),
];

fn discover_self_check_cases(root: &Path) -> TestResult<Vec<SelfCheckCase>> {
    let mut cases = Vec::new();
    collect_self_check_cases(root, root, &mut cases)?;
    cases.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(cases)
}

fn collect_self_check_cases(root: &Path, dir: &Path, cases: &mut Vec<SelfCheckCase>) -> TestResult {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if entry.file_name() != ".git" {
                collect_self_check_cases(root, &path, cases)?;
            }
        } else {
            files.push(path);
        }
    }
    let has_extension = |path: &PathBuf, wanted: &str| {
        path.extension().and_then(|ext| ext.to_str()) == Some(wanted)
    };
    if files.iter().any(|path| has_extension(path, "retrosh")) {
        return Ok(());
    }
    files.sort();
    let Some(adf_path) = files.iter().find(|path| has_extension(path, "adf")) else {
        return Ok(());
    };
    let cpus = files
        .iter()
        .filter(|path| has_extension(path, "s"))
        .filter(|source| source_uses_verdict_harness(source))
        .flat_map(|source| source_expected_cpus(source))
        .collect::<Vec<_>>();
    if cpus.is_empty() {
        return Ok(());
    }
    let rel_path = adf_path.strip_prefix(root)?.to_path_buf();
    let name = rel_path
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join("/");
    cases.push(SelfCheckCase {
        name,
        rel_path,
        adf_path: adf_path.clone(),
        cpus,
    });
    Ok(())
}

/// The CPUs an assembly source defines an expected-values array for.
fn source_expected_cpus(source: &Path) -> Vec<&'static str> {
    let text = fs::read_to_string(source).unwrap_or_default();
    SELF_CHECK_MACHINES
        .iter()
        .filter(|machine| {
            text.lines()
                .any(|line| line.trim_end().starts_with(machine.expected_label))
        })
        .map(|machine| machine.cpu)
        .collect()
}

/// Whether an assembly source includes a harness that paints the pass and
/// fail verdict colours.
fn source_uses_verdict_harness(source: &Path) -> bool {
    let Ok(text) = fs::read_to_string(source) else {
        return false;
    };
    let Some(dir) = source.parent() else {
        return false;
    };
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("include"))
        .filter_map(|rest| rest.trim().strip_prefix('"')?.split('"').next())
        .filter_map(|include| fs::read_to_string(dir.join(include)).ok())
        .any(|harness| harness.contains("#$060,") && harness.contains("#$600,"))
}

fn run_self_check(
    emulator: &str,
    kick13: &Path,
    out_root: &Path,
    case: &SelfCheckCase,
    machine: SelfCheckMachine,
) -> TestResult<String> {
    let script = ShippedScript {
        slow_ram_kb: Some(0),
        drives: 1,
        ..ShippedScript::default()
    };
    let stem = case
        .rel_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let out_base = out_root
        .join(&case.rel_path)
        .with_file_name(format!("{stem}_{}.adf", machine.cpu));
    let png_path = run_copperline(
        emulator,
        kick13,
        &out_base,
        &case.name,
        &case.adf_path,
        machine.setup,
        machine.cpu,
        &script,
        9.0,
    )?;
    // The verdict is the background colour, so any pixel of the top border
    // carries it.
    Ok(self_check_verdict(png_pixel_rgb(&png_path, 20, 20)?))
}

fn self_check_verdict(rgb: [u8; 3]) -> String {
    SELF_CHECK_VERDICTS
        .iter()
        .find(|(colour, _)| *colour == rgb)
        .map(|(_, verdict)| (*verdict).to_owned())
        .unwrap_or_else(|| format!("other:#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
}

/// Cases (`name<TAB>cpu`) that pass in `baseline` and not in `current`.
/// Both are `selfcheck.tsv` texts: `name<TAB>cpu<TAB>verdict` per line.
fn self_check_regressions(baseline: &str, current: &str) -> Vec<String> {
    let verdict_of = |text: &str, key: &str| {
        text.lines().find_map(|line| {
            let (case, verdict) = line.rsplit_once('\t')?;
            (case == key).then(|| verdict.to_owned())
        })
    };
    baseline
        .lines()
        .filter_map(|line| line.rsplit_once('\t'))
        .filter(|(_, verdict)| *verdict == "pass")
        .filter(|(key, _)| verdict_of(current, key).is_some_and(|verdict| verdict != "pass"))
        .map(|(key, _)| key.to_owned())
        .collect()
}

fn png_pixel_rgb(path: &Path, x: usize, y: usize) -> TestResult<[u8; 3]> {
    let decoder = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG dimensions overflow")?;
    let mut data = vec![0; size];
    let info = reader.next_frame(&mut data)?;
    let stride = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err("unexpected screenshot pixel format".into()),
    };
    let offset = (y * info.width as usize + x) * stride;
    let pixel = data
        .get(offset..offset + 3)
        .ok_or("screenshot smaller than the sampled position")?;
    Ok([pixel[0], pixel[1], pixel[2]])
}

fn assert_png_dimensions(path: &Path, expected_width: u32, expected_height: u32) -> TestResult {
    let decoder = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
    let reader = decoder.read_info()?;
    let info = reader.info();
    assert_eq!(
        (info.width, info.height),
        (expected_width, expected_height),
        "{}",
        path.display()
    );
    Ok(())
}

fn compare_png_bytes(expected: &Path, actual: &Path, name: &str) -> TestResult {
    let expected_bytes = fs::read(expected).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{name}: reading baseline PNG {} failed: {e}",
                expected.display()
            ),
        )
    })?;
    let actual_bytes = fs::read(actual)?;
    if expected_bytes != actual_bytes {
        return Err(io::Error::other(format!(
            "{name}: screenshot differs from baseline {}\nactual: {}",
            expected.display(),
            actual.display()
        ))
        .into());
    }
    Ok(())
}

fn kickstart_13_path() -> Option<PathBuf> {
    env_path("COPPERLINE_VAMIGATS_KICK13")
        .or_else(|| existing_path(repo_root().join("KICK13.ROM")))
        .or_else(|| existing_path(PathBuf::from("/tmp/kick13.rom")))
}

fn existing_path(path: PathBuf) -> Option<PathBuf> {
    path.exists().then_some(path)
}

fn env_path(name: &str) -> Option<PathBuf> {
    envcfg::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

fn parse_optional_usize(name: &str) -> TestResult<Option<usize>> {
    envcfg::var(name)
        .map(|s| {
            s.parse::<usize>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e).into())
        })
        .transpose()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()))
}

fn vamiga_temp_stem(case: &VAmigaTsCase) -> String {
    let mut hasher = DefaultHasher::new();
    case.rel_path.hash(&mut hasher);
    let file_stem = case
        .adf_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("case");
    format!(
        "copperline-vamigats-{}-{:016x}-{}",
        std::process::id(),
        hasher.finish(),
        shell_word_stem(file_stem)
    )
}

fn shell_word_stem(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn tail_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let start = text.len().saturating_sub(4096);
    text[start..].to_string()
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[test]
fn toml_string_escapes_paths() {
    assert_eq!(
        toml_string(r#"C:\roms\kick "1.3".rom"#),
        r#""C:\\roms\\kick \"1.3\".rom""#
    );
}

#[test]
fn discover_adf_cases_finds_nested_tests_in_sorted_order() -> TestResult {
    let root = unique_temp_dir("copperline-vamigats-discovery-test");
    let first = root.join("Agnus/Blitter/bbusy/bbusy0");
    let second = root.join("Paula/Registers/ADKCON/adkcon1");
    fs::create_dir_all(&first)?;
    fs::create_dir_all(&second)?;
    fs::write(second.join("adkcon1.adf"), [])?;
    fs::write(first.join("bbusy0.adf"), [])?;
    fs::write(first.join("bbusy0.txt"), [])?;

    let cases = discover_adf_cases(&root)?;
    let _ = fs::remove_dir_all(&root);

    assert_eq!(
        cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Agnus/Blitter/bbusy/bbusy0/bbusy0",
            "Paula/Registers/ADKCON/adkcon1/adkcon1"
        ]
    );
    Ok(())
}

#[test]
fn vamiga_retroshell_script_uses_temp_paths_and_setup() {
    let script = vamiga_retroshell_script(
        "A500_OCS_1MB",
        "68010",
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/bbusy0.adf"),
        9.0,
        "bbusy0",
        &ShippedScript::default(),
    );

    assert!(script.contains("regression setup A500_OCS_1MB /tmp/kick13.rom"));
    assert!(script.contains("cpu set REVISION 68010"));
    assert!(script.contains("regression run /tmp/bbusy0.adf"));
    assert!(script.contains("wait 9 seconds"));
    assert!(script.contains("screenshot save bbusy0"));
}

#[test]
fn shipped_cutout_is_parsed_in_both_spellings() {
    let named = parse_shipped_retrosh(
        "regression setup A500_OCS_1MB /tmp/kick13.rom\n\
         screenshot set cutout x1=300 y1=26 x2=912 y2=311\n\
         regression run /tmp/lc1.adf\n",
    );
    assert_eq!(named.cutout, Some([300, 26, 912, 311]));

    let positional = parse_shipped_retrosh("screenshot set cutout 300 26 912 311\n");
    assert_eq!(positional.cutout, Some([300, 26, 912, 311]));

    // Bounds in another order, or too few of them, are not guessed at.
    for line in [
        "screenshot set cutout y1=26 x1=300 x2=912 y2=311\n",
        "screenshot set cutout 300 26 912\n",
        "screenshot set filename lc1\n",
    ] {
        assert_eq!(parse_shipped_retrosh(line).cutout, None, "{line}");
    }
}

#[test]
fn generated_reference_is_masked_outside_the_shipped_cutout() {
    let mut raw = vec![0xFFu8; VAMIGA_DUMP_WIDTH * VAMIGA_DUMP_HEIGHT * 3];
    // Texture columns 300..912 and rows 34..311: the dump starts at texel
    // (196, 26), so the box begins 104 columns in and 8 rows down.
    mask_vamiga_dump_outside_cutout(&mut raw, [300, 34, 912, 311]);
    let texel = |col: usize, row: usize| {
        let at = (row * VAMIGA_DUMP_WIDTH + col) * 3;
        [raw[at], raw[at + 1], raw[at + 2]]
    };

    // Inside the box the picture is untouched, up to both far edges.
    assert_eq!(texel(104, 8), [0xFF; 3]);
    assert_eq!(texel(715, 284), [0xFF; 3]);
    // Left of it and above it lies vAmiga's checkerboard: 8-texel cells on
    // the texture grid, 0x22 where the cell row and column parities agree.
    assert_eq!(texel(103, 8), [0x44; 3]); // texel (299, 34): cells 37, 4
    assert_eq!(texel(95, 8), [0x22; 3]); // texel (291, 34): cells 36, 4
    assert_eq!(texel(104, 7), [0x44; 3]); // texel (300, 33): cells 37, 4
    assert_eq!(texel(0, 0), [0x44; 3]); // texel (196, 26): cells 24, 3
    assert_eq!(texel(4, 0), [0x22; 3]); // texel (200, 26): cells 25, 3
    assert_eq!(texel(0, 6), [0x22; 3]); // texel (196, 32): cells 24, 4

    // The bounds are half-open: a box that stops one texel short of the
    // right and bottom edges blanks that last column and row.
    let mut raw = vec![0xFFu8; VAMIGA_DUMP_WIDTH * VAMIGA_DUMP_HEIGHT * 3];
    mask_vamiga_dump_outside_cutout(&mut raw, [196, 26, 911, 310]);
    let at = |col: usize, row: usize| raw[(row * VAMIGA_DUMP_WIDTH + col) * 3];
    assert_eq!(at(714, 283), 0xFF);
    assert_ne!(at(715, 283), 0xFF);
    assert_ne!(at(714, 284), 0xFF);
}

#[test]
fn a1200_setup_selects_aga_ec020_and_two_megabytes_of_chip_ram() {
    let config = copperline_config(
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/fmode10.adf"),
        "A1200_2MB",
        machine_for_setup("A1200_2MB").default_cpu,
        &ShippedScript::default(),
    );

    assert!(config.contains("model = \"68EC020\""));
    assert!(config.contains("chip = \"2M\""));
    assert!(config.contains("slow = \"0\""));
    assert!(config.contains("revision = \"AGA\""));
    assert!(!config.contains("revision = \"OCS\""));
}

#[test]
fn a1200_script_without_cpu_override_uses_machine_default() -> TestResult {
    let root = unique_temp_dir("copperline-vamigats-a1200-discovery-test");
    let case_dir = root.join("Agnus/Registers/FMODE/fmode10");
    fs::create_dir_all(&case_dir)?;
    fs::write(case_dir.join("fmode10.adf"), [])?;
    fs::write(
        case_dir.join("fmode10.retrosh"),
        "regression setup A1200_2MB /tmp/kick13.rom\n\
         regression run /tmp/fmode10.adf\n\
         wait 9 seconds\n",
    )?;

    let cases = discover_adf_cases(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].setup, "A1200_2MB");
    assert_eq!(cases[0].cpu, "68EC020");
    Ok(())
}

#[test]
fn vamiga_temp_stem_keeps_shell_word_characters() {
    let case = VAmigaTsCase {
        name: "Agnus/Blitter/test case/test case".to_string(),
        rel_path: PathBuf::from("Agnus/Blitter/test case/test case.adf"),
        adf_path: PathBuf::from("/suite/Agnus/Blitter/test case/test case.adf"),
        setup: "A500_OCS_1MB".to_string(),
        cpu: "68000".to_string(),
        seconds: None,
        script: ShippedScript::default(),
        shipped_raw: None,
    };

    let stem = vamiga_temp_stem(&case);
    assert!(stem.starts_with(&format!("copperline-vamigats-{}-", std::process::id())));
    assert!(stem.ends_with("-test_case"));
    assert!(stem
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
}

#[test]
fn shipped_script_revision_keyword_matches_either_case() {
    for line in ["cpu set revision 68010", "cpu set REVISION 68010"] {
        let script = parse_shipped_retrosh(&format!(
            "regression setup A500_ECS_1MB /tmp/kick13.rom\n{line}\nregression run /tmp/x.adf\n"
        ));
        assert_eq!(script.cpu.as_deref(), Some("68010"), "{line}");
    }
}

#[test]
fn shipped_script_waits_accumulate_and_time_the_input() {
    let script = parse_shipped_retrosh(
        "regression run /tmp/x.adf\n\
         wait 16 seconds\n\
         # Press the right mouse button\n\
         mouse1 press right\n\
         wait 2 seconds\n\
         screenshot save x\n",
    );
    assert_eq!(script.total_seconds(), Some(18.0));
    assert_eq!(
        copperline_input_args(&script),
        ["--click-after", "16.000", "right", "100", "1"]
    );

    let reference = vamiga_retroshell_script(
        "A500_ECS_1MB",
        "68000",
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/x.adf"),
        18.0,
        "x",
        &script,
    );
    let tail = reference
        .lines()
        .skip_while(|line| !line.starts_with("regression run"))
        .collect::<Vec<_>>();
    assert_eq!(
        tail,
        [
            "regression run /tmp/x.adf",
            "wait 16 seconds",
            "mouse1 press right",
            "wait 2 seconds",
            "screenshot save x"
        ]
    );
}

#[test]
fn shipped_script_accepts_both_device_spellings() {
    let script = parse_shipped_retrosh(
        "regression run /tmp/x.adf\n\
         joystick 1 pull left\n\
         joystick2 pull down\n\
         keyboard press 40\n\
         wait 9 seconds\n",
    );
    assert_eq!(
        script.steps[..3],
        [
            ScriptStep::JoystickPull {
                port: 1,
                direction: "left".to_owned()
            },
            ScriptStep::JoystickPull {
                port: 2,
                direction: "down".to_owned()
            },
            ScriptStep::KeyPress { code: 40 },
        ]
    );
    // A generated reference script always uses the attached spelling.
    assert_eq!(
        script.steps[0].retroshell_line().as_deref(),
        Some("joystick1 pull left")
    );
    let args = copperline_input_args(&script);
    assert!(args.ends_with(&["--port1".to_owned(), "joystick".to_owned()]));
    assert!(args
        .windows(3)
        .any(|w| w == ["--press-after", "0.000", "0x28"]));
}

#[test]
fn shipped_script_memory_video_and_drives_reach_both_emulators() {
    let script = parse_shipped_retrosh(
        "regression setup A500_ECS_1MB /tmp/kick13.rom\n\
         mem set SLOW_RAM 0\n\
         amiga set VIDEO_FORMAT NTSC\n\
         df1 connect\n\
         df3 connect\n\
         regression run /tmp/x.adf\n\
         wait 11 seconds\n",
    );
    let config = copperline_config(
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/x.adf"),
        "A500_ECS_1MB",
        "68000",
        &script,
    );
    assert!(config.contains("slow = \"0\""));
    assert!(config.contains("video = \"NTSC\""));
    assert_eq!(copperline_input_args(&script), ["--floppy-drives", "4"]);

    let reference = vamiga_retroshell_script(
        "A500_ECS_1MB",
        "68000",
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/x.adf"),
        11.0,
        "x",
        &script,
    );
    let run_at = reference.find("regression run").unwrap();
    for line in [
        "mem set SLOW_RAM 0",
        "amiga set VIDEO_FORMAT NTSC",
        "df3 connect",
    ] {
        assert!(reference.find(line).is_some_and(|at| at < run_at), "{line}");
    }
    // Denise detects collisions whether or not a script asks vAmiga to.
    assert!(reference.contains("denise set CLX_SPR_PLF true"));
}

#[test]
fn a_script_without_memory_or_video_lines_keeps_the_machine_defaults() {
    let config = copperline_config(
        Path::new("/tmp/kick13.rom"),
        Path::new("/tmp/x.adf"),
        "A500_ECS_1MB",
        "68000",
        &parse_shipped_retrosh("regression run /tmp/x.adf\nwait 9 seconds\n"),
    );
    assert!(config.contains("slow = \"512K\""));
    assert!(config.contains("video = \"PAL\""));
}

#[test]
fn discovery_pairs_a_script_with_its_shipped_reference() -> TestResult {
    let root = unique_temp_dir("copperline-vamigats-shipped-ref-test");
    let case_dir = root.join("Agnus/Copper/lc/lc1");
    fs::create_dir_all(&case_dir)?;
    fs::write(case_dir.join("lc1.adf"), [])?;
    for variant in ["lc1_ocs", "lc1_ecs"] {
        fs::write(
            case_dir.join(format!("{variant}.retrosh")),
            "regression setup A500_OCS_1MB /tmp/kick13.rom\n\
             regression run /tmp/lc1.adf\n\
             wait 9 seconds\n",
        )?;
    }
    fs::write(case_dir.join("lc1_ocs.raw"), [])?;

    let cases = discover_adf_cases(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].name, "Agnus/Copper/lc/lc1/lc1_ecs");
    assert_eq!(cases[0].shipped_raw, None);
    assert!(cases[1]
        .shipped_raw
        .as_deref()
        .is_some_and(|raw| raw.ends_with("lc1_ocs.raw")));
    Ok(())
}

#[test]
fn self_check_discovery_wants_a_harness_expectations_and_no_script() -> TestResult {
    let root = unique_temp_dir("copperline-vamigats-selfcheck-test");
    let family = root.join("CPU/Exceptions/StackFrame");
    let judged = family.join("addrerr13");
    let scripted = family.join("stackframe1");
    let plain = root.join("CIA/TOD/tod1");
    for dir in [&judged, &scripted, &plain] {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        family.join("stackframe.i"),
        "\tmove.w  #$060,$DFF180\n\tmove.w  #$600,$DFF180\n",
    )?;
    // A harness-judged test that carries no per-CPU expectations (the 68030
    // MMU tests are of this kind) is not one of ours either.
    let unlabelled = family.join("mmu1");
    fs::create_dir_all(&unlabelled)?;
    for (dir, name, expectations) in [
        (
            &judged,
            "addrerr13",
            "expected000:\n\tdc.w 0\nexpected020:\n",
        ),
        (&scripted, "stackframe1", "expected000:\n"),
        (&unlabelled, "mmu1", "expected:\n"),
    ] {
        fs::write(dir.join(format!("{name}.adf")), [])?;
        fs::write(
            dir.join(format!("{name}.s")),
            format!("\tinclude \"../stackframe.i\"\n{expectations}"),
        )?;
    }
    fs::write(scripted.join("stackframe1.retrosh"), "wait 9 seconds\n")?;
    fs::write(plain.join("tod1.adf"), [])?;
    fs::write(
        plain.join("tod1.s"),
        "\tmove.w  #$060,$DFF180\nexpected000:\n",
    )?;

    let cases = discover_self_check_cases(&root)?;
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        cases.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["CPU/Exceptions/StackFrame/addrerr13/addrerr13"]
    );
    // It runs on the CPUs it has an array for, and no others.
    assert_eq!(cases[0].cpus, ["68000", "68EC020"]);
    Ok(())
}

#[test]
fn self_check_verdict_reads_the_harness_background_colour() {
    assert_eq!(self_check_verdict([0x00, 0x66, 0x00]), "pass");
    assert_eq!(self_check_verdict([0x66, 0x00, 0x00]), "fail");
    assert_eq!(self_check_verdict([0x00, 0xFF, 0x00]), "no-exception");
    assert_eq!(self_check_verdict([0x00, 0x00, 0x88]), "other:#000088");
}

#[test]
fn self_check_regressions_lists_only_lost_passes() {
    let baseline = "a/a\t68000\tpass\na/a\t68010\tfail\nb/b\t68000\tpass\nc/c\t68000\tpass\n";
    let current = "a/a\t68000\tpass\na/a\t68010\tpass\nb/b\t68000\tfail\n";
    assert_eq!(self_check_regressions(baseline, current), ["b/b\t68000"]);
}

#[test]
fn run_parallel_visits_every_item_and_reports_a_failure() {
    let seen = AtomicUsize::new(0);
    let items = (0..40).collect::<Vec<_>>();
    run_parallel(&items, 4, |_| {
        seen.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen.load(Ordering::Relaxed), 40);

    let error = run_parallel(&items, 4, |item| {
        if *item == 7 {
            Err("item seven".into())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "item seven");
}
