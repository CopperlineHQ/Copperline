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

/// Where a `--terminal` session's log output goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogTarget {
    /// `--terminal-log PATH`: this file, replaced if it exists.
    Named(PathBuf),
    /// A new `copperline-terminal-<pid>.log` in this directory, readable by
    /// the owner only. An existing file of that name is never opened: the
    /// directory is the shared temporary one.
    Private(PathBuf),
}

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
    /// Where this session's log goes.
    pub fn log_target(&self) -> LogTarget {
        match &self.log_path {
            Some(path) => LogTarget::Named(path.clone()),
            None => LogTarget::Private(std::env::temp_dir()),
        }
    }

    /// The writer thread's minimum spacing between frames. A rate too
    /// small for its interval to fit a `Duration` gets the longest one.
    pub fn min_frame_interval(&self) -> Option<std::time::Duration> {
        use std::time::Duration;
        self.max_fps
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .map(|fps| Duration::try_from_secs_f64(1.0 / f64::from(fps)).unwrap_or(Duration::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--terminal-fps` takes any positive number; one too small for its
    /// frame interval to fit a `Duration` means "as slowly as can be said".
    #[test]
    fn a_tiny_frame_rate_is_a_very_long_interval() {
        let fps = |max_fps| Options {
            max_fps: Some(max_fps),
            log_path: None,
        };
        assert_eq!(
            fps(25.0).min_frame_interval(),
            Some(std::time::Duration::from_millis(40))
        );
        assert_eq!(
            fps(1e-20).min_frame_interval(),
            Some(std::time::Duration::MAX)
        );
        assert_eq!(
            fps(f32::MIN_POSITIVE).min_frame_interval(),
            Some(std::time::Duration::MAX)
        );
        assert_eq!(Options::default().min_frame_interval(), None);
    }
}
