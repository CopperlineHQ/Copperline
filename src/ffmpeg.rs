// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding the host's ffmpeg.
//!
//! Copperline links no video codec of its own: an MP4 recording is made
//! from the lossless AVI the recorder writes by running the host's ffmpeg
//! on it (`crate::recorder::mp4`), and the Video Toaster's inputs and
//! Flyer conversions decode movies with it. This is the one place that
//! decides which ffmpeg that is.

use std::path::{Path, PathBuf};

/// The executable's file name on this host.
fn exe_name() -> &'static str {
    if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

/// Directories package managers install ffmpeg into that a desktop
/// launch may not have on its `PATH`: an app started from the macOS Dock
/// or Finder gets `/usr/bin:/bin:/usr/sbin:/sbin`, which misses Homebrew
/// and MacPorts.
fn well_known_dirs() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
    } else if cfg!(unix) {
        &["/usr/local/bin", "/usr/bin"]
    } else {
        &[]
    }
}

/// Where ffmpeg is, if anywhere. In order: `COPPERLINE_FFMPEG` (a one-run
/// override), `configured` (`[recording] ffmpeg`), an `ffmpeg` in one of
/// `search` (the Toaster passes its files folder), beside this program,
/// on the `PATH`, then the package managers' usual directories. A
/// configured path that names no file is reported and passed over.
pub fn find(configured: Option<&Path>, search: &[&Path]) -> Option<PathBuf> {
    if cfg!(all(target_arch = "wasm32", target_os = "unknown")) {
        // A browser build has no host programs to run.
        return None;
    }
    if let Some(p) = crate::envcfg::var_os("COPPERLINE_FFMPEG").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
        log::warn!(
            "COPPERLINE_FFMPEG names no file ({}); looking for ffmpeg elsewhere",
            p.display()
        );
    }
    if let Some(p) = configured {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
        log::warn!(
            "[recording] ffmpeg names no file ({}); looking for ffmpeg elsewhere",
            p.display()
        );
    }
    candidates(search, std::env::var_os("PATH").as_deref())
        .into_iter()
        .find(|p| p.is_file())
}

/// Every place [`find`] looks after the explicit settings, in order.
fn candidates(search: &[&Path], path_var: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let exe = exe_name();
    let mut out: Vec<PathBuf> = search.iter().map(|dir| dir.join(exe)).collect();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        out.push(dir.join(exe));
    }
    if let Some(path) = path_var {
        out.extend(std::env::split_paths(path).map(|d| d.join(exe)));
    }
    out.extend(well_known_dirs().iter().map(|d| Path::new(d).join(exe)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_dirs_come_before_the_path_and_well_known_dirs_last() {
        let search = Path::new("/media/root");
        let path_var = std::env::join_paths(["/first/bin", "/second/bin"]).unwrap();
        let list = candidates(&[search], Some(&path_var));
        let exe = exe_name();
        assert_eq!(list[0], search.join(exe));
        let first = list
            .iter()
            .position(|p| p == &Path::new("/first/bin").join(exe))
            .expect("PATH entries are searched");
        let second = list
            .iter()
            .position(|p| p == &Path::new("/second/bin").join(exe))
            .unwrap();
        assert!(first < second, "PATH order is kept");
        for dir in well_known_dirs() {
            let at = list
                .iter()
                .rposition(|p| p == &Path::new(dir).join(exe))
                .expect("well-known directories are searched");
            assert!(at > second, "{dir} is searched after the PATH");
        }
    }

    #[test]
    fn a_configured_file_is_used_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("my-ffmpeg");
        std::fs::write(&fake, b"").unwrap();
        // COPPERLINE_FFMPEG outranks it, so only check when it is unset.
        if crate::envcfg::var_os("COPPERLINE_FFMPEG").is_none() {
            assert_eq!(find(Some(&fake), &[]), Some(fake));
        }
    }
}
