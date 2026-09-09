// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end `--run PROG --coverage FILE`: the built emulator boots the
//! bundled AROS ROM, runs the committed `guest/dap-test/hello` probe
//! (amiga-gcc 6.5, DWARF in its debug hunk), and the run ends by itself
//! when the program exits with its lcov file written. No local assets are
//! needed. Ignored like the rest of this directory because it boots the
//! emulator; run it with
//!
//! ```sh
//! cargo test --release --test coverage_run -- --ignored
//! ```

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tail(text: &[u8], line_count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(line_count);
    lines[start..].join("\n")
}

/// The `FNDA:count,name` value for `name`.
fn fnda(info: &str, name: &str) -> u64 {
    let needle = format!(",{name}");
    info.lines()
        .find_map(|line| {
            line.strip_prefix("FNDA:")
                .and_then(|rest| rest.strip_suffix(needle.as_str()))
                .and_then(|count| count.parse().ok())
        })
        .unwrap_or_else(|| panic!("no FNDA record for {name}:\n{info}"))
}

#[test]
#[ignore = "boots the bundled AROS ROM; release build only"]
fn run_coverage_writes_lcov_with_the_probe_functions() {
    if cfg!(debug_assertions) {
        eprintln!(
            "skipping --coverage end-to-end run; run with --release \
             (a debug emulator is far too slow for a full AROS boot + --run staging)"
        );
        return;
    }
    let root = repo_root();
    let scratch =
        std::env::temp_dir().join(format!("copperline-coverage-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let program_dir = scratch.join("program");
    let config_home = scratch.join("config-home");
    std::fs::create_dir_all(&program_dir).unwrap();
    std::fs::create_dir_all(&config_home).unwrap();
    let program = program_dir.join("hello");
    std::fs::copy(root.join("guest/dap-test/hello"), &program).unwrap();
    let out = scratch.join("out/hello.info");

    // The probe's DWARF records the build machine's source directory;
    // map it to the checkout so the file names a real path.
    let fixture_debug =
        copperline::debuginfo::DebugInfo::load(&std::fs::read(&program).unwrap(), None)
            .expect("loading fixture debug information");
    let recorded_dir = fixture_debug
        .files
        .iter()
        .find(|file| file.path.ends_with("/guest/dap-test/hello.c"))
        .and_then(|file| std::path::Path::new(&file.path).parent())
        .map(|dir| dir.to_string_lossy().into_owned())
        .expect("fixture DWARF source directory");
    let mapped_dir = root.join("guest/dap-test").display().to_string();

    let output = Command::new(env!("CARGO_BIN_EXE_copperline"))
        .current_dir(&root)
        .env("RUST_LOG", "copperline=warn")
        .env("COPPERLINE_AROS_DIR", root.join("assets/aros"))
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("--factory")
        .arg("--noaudio")
        .arg("--run")
        .arg(&program)
        .arg("--coverage")
        .arg(&out)
        .arg("--coverage-source-map")
        .arg(format!("{recorded_dir}={mapped_dir}"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Copperline exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        tail(&output.stdout, 40),
        tail(&output.stderr, 80),
    );

    let info = std::fs::read_to_string(&out).unwrap_or_else(|e| {
        panic!(
            "no coverage file at {}: {e}\nstderr:\n{}",
            out.display(),
            tail(&output.stderr, 80)
        )
    });
    assert!(info.contains("hello exited; final counts"), "{info}");
    assert!(info.contains("TN:hello\n"), "{info}");
    assert!(
        info.contains(&format!("SF:{mapped_dir}/hello.c\n")),
        "{info}"
    );
    // entry() runs once, calls scale() for i = 1..=3, and add() from each
    // scale() plus once directly.
    assert_eq!(fnda(&info, "entry"), 1, "{info}");
    assert_eq!(fnda(&info, "scale"), 3, "{info}");
    assert_eq!(fnda(&info, "add"), 4, "{info}");
    assert!(info.contains("FNF:3\nFNH:3\n"), "{info}");
    // Every line of the probe's straight-line C runs; the two RC-20 lines
    // behind the missing-dos.library check do not.
    let lf: u64 = info
        .lines()
        .find_map(|l| l.strip_prefix("LF:"))
        .and_then(|v| v.parse().ok())
        .expect("LF");
    let lh: u64 = info
        .lines()
        .find_map(|l| l.strip_prefix("LH:"))
        .and_then(|v| v.parse().ok())
        .expect("LH");
    assert!(
        lf > 20 && lh + 4 >= lf && lh < lf,
        "LF {lf} LH {lh}\n{info}"
    );
    // The accounting names everything the CPU retired: the OS ran far more
    // instructions than the probe, and nothing in the program went
    // unmapped except its startup stub.
    let outside: u64 = info
        .lines()
        .find_map(|l| {
            l.strip_prefix("# ")?
                .split_once(" instruction(s) outside the program")
                .map(|(n, _)| n.parse().ok())?
        })
        .expect("outside summary");
    assert!(outside > 100_000, "{info}");
    let _ = std::fs::remove_dir_all(&scratch);
}
