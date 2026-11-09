// SPDX-License-Identifier: GPL-3.0-or-later

//! Terminal display: show the emulated machine inside a terminal emulator and
//! drive it from the terminal's keyboard and mouse, with no window server.
//!
//! The picture goes out over the Kitty graphics protocol and input comes back
//! through the Kitty keyboard protocol (key press *and* release reports, which
//! the Amiga keyboard needs) and SGR pixel mouse reports. kitty, Ghostty and
//! WezTerm speak both, so a session on a remote machine can be watched and
//! played over plain SSH.
//!
//! The modules here are the frontend-independent halves: [`graphics`] encodes
//! frames and the status line into escape sequences, [`input`] parses what the
//! terminal sends back, and `tty` (Unix only) owns the terminal device, its raw
//! mode, and the writer thread that keeps a slow link from stalling emulation.
//! The session driver that connects them to the machine lives with the window
//! frontend (`video::window::terminal`), which it shares its input handling
//! with.

pub mod graphics;
pub mod input;
#[cfg(unix)]
pub mod tty;

use std::path::PathBuf;

/// `--terminal` session options from the command line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// `--terminal-fps N`: send at most N frames a second. Without it every
    /// new frame goes out that the link has room for; the rest are dropped.
    pub max_fps: Option<f32>,
    /// `--terminal-log PATH`: where log output goes while the terminal shows
    /// the machine (default: `copperline-terminal-<pid>.log` in the
    /// temporary directory).
    pub log_path: Option<PathBuf>,
}

impl Options {
    /// The log file this session writes to.
    pub fn resolved_log_path(&self) -> PathBuf {
        self.log_path.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("copperline-terminal-{}.log", std::process::id()))
        })
    }

    /// The writer thread's minimum spacing between frames.
    pub fn min_frame_interval(&self) -> Option<std::time::Duration> {
        self.max_fps
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .map(|fps| std::time::Duration::from_secs_f64(1.0 / f64::from(fps)))
    }
}
