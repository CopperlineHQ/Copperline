// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent crash reports from the process panic hook.
//!
//! A panic already prints to stderr, but that is invisible for the common
//! double-click launch: on Windows the console window closes with the
//! process, and desktop launchers on every platform discard stderr. The
//! hook installed here therefore also writes the panic message and a
//! backtrace to `copperline-crash.txt` so users can attach it to a bug
//! report. The file lands next to the executable (the natural spot for the
//! portable Windows bundle), falling back to the working directory and then
//! the system temporary directory when that location is not writable
//! (installed Homebrew/AppImage/Flatpak layouts).

use std::backtrace::Backtrace;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Crash-report filename, created in the first writable candidate
/// directory (see [`install`]).
pub const CRASH_FILE_NAME: &str = "copperline-crash.txt";

/// Serializes concurrently panicking threads and remembers where this
/// process wrote its report: the first panic picks a location, truncating
/// a stale report left by an earlier run, and later panics in the same
/// process append to that same file even if the candidate list would
/// resolve differently by then (the working directory can change at
/// runtime).
static REPORT_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Install the process panic hook: print the panic to stderr, persist a
/// crash report, and chain to the previously installed hook.
pub fn install() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("\n!!! PANIC: {info}");
        match write_report(&candidate_dirs(), &format_report(&info.to_string())) {
            Some(path) => eprintln!("crash report written to {}", path.display()),
            None => eprintln!("could not write a crash report file"),
        }
        prev(info);
    }));
}

/// Render one report: build/host identification, the panic message (with
/// its source location), and a backtrace. The backtrace is captured
/// independently of RUST_BACKTRACE -- crash reporters cannot be asked to
/// reproduce the crash with the variable set.
fn format_report(panic_display: &str) -> String {
    format!(
        "Copperline {} ({} {})\ntime: {}\nthread: {}\n\n{}\n\nbacktrace:\n{}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        crate::timestamp::compact_now(),
        std::thread::current().name().unwrap_or("<unnamed>"),
        panic_display,
        Backtrace::force_capture(),
    )
}

/// Where to try writing the report, in preference order.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd);
    }
    dirs.push(std::env::temp_dir());
    dirs
}

/// Write `report` to [`CRASH_FILE_NAME`] in the first directory that
/// accepts it, returning the path written.
fn write_report(dirs: &[PathBuf], report: &str) -> Option<PathBuf> {
    write_report_locked(&REPORT_PATH, dirs, report)
}

/// Testable core of [`write_report`]: `state` carries the chosen report
/// path so tests get isolated instances. A poisoned lock is taken
/// anyway -- the path stays usable, and the alternative is dropping the
/// report.
fn write_report_locked(
    state: &Mutex<Option<PathBuf>>,
    dirs: &[PathBuf],
    report: &str,
) -> Option<PathBuf> {
    let mut chosen = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = chosen.as_ref() {
        if write_file(path, true, report).is_ok() {
            return Some(path.clone());
        }
        // The chosen location vanished mid-run; fall through to pick anew.
    }
    for dir in dirs {
        let path = dir.join(CRASH_FILE_NAME);
        if write_file(&path, false, report).is_ok() {
            *chosen = Some(path.clone());
            return Some(path);
        }
    }
    None
}

fn write_file(path: &Path, append: bool, report: &str) -> std::io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true);
    if append {
        opts.append(true);
    } else {
        opts.truncate(true);
    }
    let mut file = opts.open(path)?;
    file.write_all(report.as_bytes())?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copperline-crashlog-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// A directory that cannot accept the report: a child of a path that
    /// does not exist (OpenOptions::create does not create parents).
    fn unwritable_dir() -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "copperline-crashlog-missing-{}",
                std::process::id()
            ))
            .join("nonexistent")
    }

    #[test]
    fn first_report_truncates_stale_and_later_appends() {
        let dir = temp_dir("truncate");
        let path = dir.join(CRASH_FILE_NAME);
        std::fs::write(&path, "stale report from an earlier run").unwrap();
        let state = Mutex::new(None);
        let got =
            write_report_locked(&state, std::slice::from_ref(&dir), "first\n").expect("written");
        assert_eq!(got, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
        write_report_locked(&state, std::slice::from_ref(&dir), "second\n").expect("written");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_past_an_unwritable_directory() {
        let dir = temp_dir("fallback");
        let state = Mutex::new(None);
        let got = write_report_locked(&state, &[unwritable_dir(), dir.clone()], "report")
            .expect("written");
        assert_eq!(got, dir.join(CRASH_FILE_NAME));
        assert_eq!(
            std::fs::read_to_string(dir.join(CRASH_FILE_NAME)).unwrap(),
            "report"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn later_panic_stays_with_the_chosen_file() {
        // A second panic must append to the file the first one picked even
        // when the candidate list has since changed (e.g. the working
        // directory moved), not start a second report elsewhere.
        let first = temp_dir("chosen");
        let other = temp_dir("other");
        let state = Mutex::new(None);
        write_report_locked(&state, std::slice::from_ref(&first), "first\n").expect("written");
        let got = write_report_locked(&state, &[other.clone(), first.clone()], "second\n")
            .expect("written");
        assert_eq!(got, first.join(CRASH_FILE_NAME));
        assert_eq!(
            std::fs::read_to_string(first.join(CRASH_FILE_NAME)).unwrap(),
            "first\nsecond\n"
        );
        assert!(!other.join(CRASH_FILE_NAME).exists());
        std::fs::remove_dir_all(&first).ok();
        std::fs::remove_dir_all(&other).ok();
    }

    #[test]
    fn vanished_chosen_file_location_is_repicked() {
        let first = temp_dir("vanishing");
        let fallback = temp_dir("repick");
        let state = Mutex::new(None);
        write_report_locked(&state, std::slice::from_ref(&first), "first\n").expect("written");
        std::fs::remove_dir_all(&first).unwrap();
        let got = write_report_locked(&state, std::slice::from_ref(&fallback), "second\n")
            .expect("written");
        assert_eq!(got, fallback.join(CRASH_FILE_NAME));
        assert_eq!(
            std::fs::read_to_string(fallback.join(CRASH_FILE_NAME)).unwrap(),
            "second\n"
        );
        assert_eq!(*state.lock().unwrap(), Some(got));
        std::fs::remove_dir_all(&fallback).ok();
    }

    #[test]
    fn no_writable_candidate_returns_none() {
        let state = Mutex::new(None);
        assert!(write_report_locked(&state, &[unwritable_dir()], "report").is_none());
        assert!(state.lock().unwrap().is_none());
    }

    #[test]
    fn report_names_the_build_and_the_panic() {
        let report = format_report("panicked at src/foo.rs:1:1:\nboom");
        assert!(report.contains(env!("CARGO_PKG_VERSION")));
        assert!(report.contains(std::env::consts::OS));
        assert!(report.contains("boom"));
        assert!(report.contains("backtrace:"));
    }
}
