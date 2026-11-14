//! Host resources this process has been asked to open.
//!
//! A save state records the host paths behind its hard-drive, floppy and CD
//! images, its `[[filesys]]` directories and its plugin modules, plus the
//! network backend of a fitted Ethernet board, and restoring the state
//! reopens them. A state file is trusted like a configuration file (see
//! SECURITY.md), so that is allowed, but a state that reaches for something
//! the running session never opened is worth telling the user about. The
//! places that open a host resource for the configuration, the command line
//! or the UI record it here; the places that reopen one for a restored
//! state ask whether it was already recorded and warn when it was not.
//!
//! The registry is process-wide because the answer has to survive the
//! machine being replaced by the state that is being loaded.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

/// What kind of host resource a path or name stands for. The kind is part of
/// the identity: a file opened as a read-only CD image has not been opened
/// as a writable hard-drive image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// A hard-drive image, opened for reading and writing.
    HardDrive,
    /// A floppy image the guest's writes are written back to.
    Floppy,
    /// A CD image or one of the files a cue sheet names; read only.
    Cd,
    /// A tape cartridge: a host file, or a host directory of tape files,
    /// that the guest can write, cut short and erase.
    Tape,
    /// A `[[filesys]]` host directory the guest can only read.
    DirectoryReadOnly,
    /// A `[[filesys]]` host directory the guest can change.
    DirectoryReadWrite,
    /// A WebAssembly board module.
    #[cfg_attr(not(feature = "wasm-boards"), allow(dead_code))]
    Plugin,
    /// A host network backend (the detail is its description, not a path).
    Network,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::HardDrive => "hard-drive image (read/write)",
            Self::Floppy => "floppy image (read/write)",
            Self::Cd => "CD image",
            Self::Tape => "tape cartridge (read/write)",
            Self::DirectoryReadOnly => "host directory (read-only)",
            Self::DirectoryReadWrite => "host directory (read/write)",
            Self::Plugin => "plugin module",
            Self::Network => "network backend",
        }
    }
}

#[derive(Default)]
struct Registry(BTreeSet<(Kind, String)>);

impl Registry {
    const fn new() -> Self {
        Self(BTreeSet::new())
    }

    /// Record a resource the session opened.
    fn open(&mut self, kind: Kind, detail: &str) {
        self.0.insert((kind, detail.to_string()));
        // Write access to a directory includes read access to it.
        if kind == Kind::DirectoryReadWrite {
            self.0.insert((Kind::DirectoryReadOnly, detail.to_string()));
        }
    }

    /// Record a resource a restored state names, returning whether the
    /// session had opened it before.
    fn restore(&mut self, kind: Kind, detail: &str) -> bool {
        let known = self.0.contains(&(kind, detail.to_string()));
        if !known {
            // Warn once per resource, not once per load of the same state.
            self.open(kind, detail);
        }
        known
    }
}

static OPENED: Mutex<Registry> = Mutex::new(Registry::new());

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    // The set stays usable after a panic elsewhere: every update is a
    // single insert.
    OPENED.lock().unwrap_or_else(|e| e.into_inner())
}

/// The configuration, command line or UI opened the resource at `path`.
pub(crate) fn opened(kind: Kind, path: &Path) {
    registry().open(kind, &path.to_string_lossy());
}

/// As [`opened`], for a resource that is described rather than located.
pub(crate) fn opened_named(kind: Kind, detail: &str) {
    registry().open(kind, detail);
}

/// A restored state names the resource at `path`. Warn when the running
/// session had not opened it.
pub(crate) fn restored(kind: Kind, path: &Path) {
    restored_named(kind, &path.to_string_lossy());
}

/// As [`restored`], for a resource that is described rather than located.
pub(crate) fn restored_named(kind: Kind, detail: &str) {
    if !registry().restore(kind, detail) {
        log::warn!(
            "save state: the state names {} {detail}, which this session had not opened; \
             a state file has the same access to the host as a configuration file",
            kind.label()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restored_resource_is_known_only_if_the_session_opened_it() {
        let mut registry = Registry::new();
        registry.open(Kind::HardDrive, "work.hdf");
        assert!(registry.restore(Kind::HardDrive, "work.hdf"));
        assert!(!registry.restore(Kind::HardDrive, "other.hdf"));
        // Reported once: a second load of the same state stays quiet.
        assert!(registry.restore(Kind::HardDrive, "other.hdf"));
    }

    #[test]
    fn the_kind_of_access_is_part_of_the_identity() {
        let mut registry = Registry::new();
        registry.open(Kind::Cd, "disc.iso");
        assert!(!registry.restore(Kind::HardDrive, "disc.iso"));
    }

    #[test]
    fn a_read_only_directory_does_not_cover_a_writable_restore() {
        let mut registry = Registry::new();
        registry.open(Kind::DirectoryReadOnly, "share");
        assert!(!registry.restore(Kind::DirectoryReadWrite, "share"));

        let mut registry = Registry::new();
        registry.open(Kind::DirectoryReadWrite, "share");
        assert!(registry.restore(Kind::DirectoryReadOnly, "share"));
        assert!(registry.restore(Kind::DirectoryReadWrite, "share"));
    }
}
