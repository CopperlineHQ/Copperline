use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    /// CPU revision from the script's `cpu set revision` line (68000 when
    /// absent); mirrored onto both emulators.
    cpu: String,
    /// `wait N seconds` from the shipped script (COPPERLINE_VAMIGATS_SECONDS
    /// still overrides globally when set).
    seconds: Option<f32>,
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
    let Some(root) = env_path("COPPERLINE_VAMIGATS_DIR") else {
        eprintln!("skipping vAmigaTS run; set COPPERLINE_VAMIGATS_DIR to a vAmigaTS checkout");
        return Ok(());
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
        return Ok(());
    };
    let kick13 = kick13.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("canonicalizing Kickstart 1.3 ROM {}: {e}", kick13.display()),
        )
    })?;

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
    // each case uses its shipped script's wait time (default 9s).
    let seconds_override = envcfg::var("COPPERLINE_VAMIGATS_SECONDS")
        .map(|s| s.parse::<f32>())
        .transpose()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let out_root = env_path("COPPERLINE_VAMIGATS_OUT")
        .unwrap_or_else(|| unique_temp_dir("copperline-vamigats"));
    fs::create_dir_all(&out_root)?;
    let out_root = out_root.canonicalize()?;
    let baseline_root = env_path("COPPERLINE_VAMIGATS_BASELINE");
    let vamiga_reference =
        env_path("COPPERLINE_VAMIGATS_VAMIGA").map(|executable| VAmigaReference {
            executable,
            setup_override: envcfg::var("COPPERLINE_VAMIGATS_VAMIGA_SETUP"),
        });

    eprintln!(
        "running {} vAmigaTS case(s) from {}; output {}",
        cases.len(),
        root.display(),
        out_root.display()
    );
    for case in cases {
        let seconds = seconds_override.or(case.seconds).unwrap_or(9.0);
        run_case(
            env!("CARGO_BIN_EXE_copperline"),
            &kick13,
            &out_root,
            baseline_root.as_deref(),
            vamiga_reference.as_ref(),
            seconds,
            &case,
        )?;
    }
    Ok(())
}

fn run_case(
    emulator: &str,
    kick13: &Path,
    out_root: &Path,
    baseline_root: Option<&Path>,
    vamiga_reference: Option<&VAmigaReference>,
    seconds: f32,
    case: &VAmigaTsCase,
) -> TestResult {
    let mut cfg_path = out_root.join(&case.rel_path);
    cfg_path.set_extension("toml");
    let mut png_path = out_root.join(&case.rel_path);
    png_path.set_extension("png");

    if let Some(parent) = cfg_path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Run against a scratch copy of the ADF, mounted writable: vAmiga's
    // regression flow copies the disk to /tmp and inserts it unprotected,
    // and several Drive/step and Drive/read cases visualize the CIA-A
    // /WPRO line, so a write-protected mount diverges from the reference
    // (and from the shipped real-hardware photos).
    let mut disk_path = out_root.join(&case.rel_path);
    disk_path.set_extension("df0.adf");
    fs::copy(&case.adf_path, &disk_path)?;
    fs::write(
        &cfg_path,
        copperline_config(kick13, &disk_path, &case.setup, &case.cpu),
    )?;

    let output = Command::new(emulator)
        .env("COPPERLINE_HCENTER", "0")
        .current_dir(repo_root())
        .env("RUST_LOG", "copperline=warn")
        .arg("--noaudio")
        .arg("--config")
        .arg(&cfg_path)
        .arg("--screenshot-after")
        .arg(format!("{seconds:.3}"))
        .arg(&png_path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{} exited with {}\nstdout tail:\n{}\nstderr tail:\n{}",
            case.name,
            output.status,
            tail_text(&output.stdout),
            tail_text(&output.stderr)
        ))
        .into());
    }

    // A screenshot of any other size means the presentation path changed
    // shape: 716x540 is the TV-glass picture (the captured aperture
    // resampled onto the 4:3 glass), and COPPERLINE_SHOT_RAW saves the
    // woven native framebuffer instead (716x570 = the vAmiga regression
    // cutout with line doubling).
    if envcfg::flag("COPPERLINE_SHOT_RAW") {
        assert_png_dimensions(&png_path, 716, 570)?;
    } else {
        assert_png_dimensions(&png_path, 716, 540)?;
    }
    if let Some(baseline_root) = baseline_root {
        let mut expected = baseline_root.join(&case.rel_path);
        expected.set_extension("png");
        compare_png_bytes(&expected, &png_path, &case.name)?;
    }
    if let Some(vamiga_reference) = vamiga_reference {
        run_vamiga_reference(vamiga_reference, kick13, out_root, seconds, case)?;
    }
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

        let raw = fs::read(&tmp_raw).map_err(|e| {
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
        fs::write(&raw_path, raw)?;
        Ok(())
    })();

    let _ = fs::remove_file(tmp_adf);
    let _ = fs::remove_file(tmp_kick);
    let _ = fs::remove_file(tmp_raw);
    result
}

fn copperline_config(kick13: &Path, adf: &Path, setup: &str, cpu: &str) -> String {
    // Mirror the vAmiga regression setup: *_ECS_*/*_PLUS_* machines use the
    // ECS chipset, everything else OCS; the CPU revision comes from the
    // shipped script's `cpu set revision` line.
    let revision = if setup.contains("_ECS_") || setup.contains("_PLUS_") {
        "ECS"
    } else {
        "OCS"
    };
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
chip = "512K"
fast = "0"
slow = "512K"

[chipset]
revision = "{revision}"
video = "PAL"

[floppy.df0]
path = {}
write_protected = false
"#,
        toml_string(&kick13.to_string_lossy()),
        toml_string(&adf.to_string_lossy())
    )
}

fn discover_adf_cases(root: &Path) -> TestResult<Vec<VAmigaTsCase>> {
    let mut cases = Vec::new();
    collect_adf_cases(root, root, &mut cases)?;
    cases.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(cases)
}

/// Parse a shipped vAmigaTS RetroShell regression script: the machine setup,
/// an optional `cpu set revision` line, the ADF it runs, and the wait time.
fn parse_shipped_retrosh(
    text: &str,
) -> (Option<String>, Option<String>, Option<String>, Option<f32>) {
    let mut setup = None;
    let mut cpu = None;
    let mut adf = None;
    let mut seconds = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("regression setup ") {
            setup = rest.split_whitespace().next().map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("cpu set revision ") {
            cpu = rest.split_whitespace().next().map(str::to_owned);
        } else if let Some(rest) = line.strip_prefix("regression run ") {
            adf = rest
                .split_whitespace()
                .next()
                .and_then(|p| Path::new(p).file_name())
                .map(|n| n.to_string_lossy().into_owned());
        } else if let Some(rest) = line.strip_prefix("wait ") {
            seconds = rest.split_whitespace().next().and_then(|n| n.parse().ok());
        }
    }
    (setup, cpu, adf, seconds)
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
                let text = fs::read_to_string(&path)?;
                let (setup, cpu, adf_name, seconds) = parse_shipped_retrosh(&text);
                let Some(adf_name) = adf_name else { continue };
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
                cases.push(VAmigaTsCase {
                    name,
                    rel_path: rel_path.with_extension("adf"),
                    adf_path,
                    setup: setup.unwrap_or_else(|| "A500_OCS_1MB".to_owned()),
                    cpu: cpu.unwrap_or_else(|| "68000".to_owned()),
                    seconds,
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
) -> String {
    // vAmiga 4.4's RetroShell registers option setters under the option's
    // uppercase key: `cpu set REVISION 68010` (the suite's shipped scripts
    // use an older lowercase syntax).
    let cpu_line = if cpu == "68000" {
        String::new()
    } else {
        format!("cpu set REVISION {cpu}\n")
    };
    format!(
        "# Regression reference script generated by Copperline\n\
         regression setup {setup} {}\n\
         {cpu_line}\
         regression run {}\n\
         wait {} seconds\n\
         screenshot save {screenshot_stem}\n",
        kick13.display(),
        adf.display(),
        // RetroShell's wait takes an integer second count.
        seconds.ceil() as u32
    )
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
    );

    assert!(script.contains("regression setup A500_OCS_1MB /tmp/kick13.rom"));
    assert!(script.contains("cpu set REVISION 68010"));
    assert!(script.contains("regression run /tmp/bbusy0.adf"));
    assert!(script.contains("wait 9 seconds"));
    assert!(script.contains("screenshot save bbusy0"));
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
    };

    let stem = vamiga_temp_stem(&case);
    assert!(stem.starts_with(&format!("copperline-vamigats-{}-", std::process::id())));
    assert!(stem.ends_with("-test_case"));
    assert!(stem
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'));
}
