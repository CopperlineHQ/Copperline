// SPDX-License-Identifier: GPL-3.0-or-later

//! `AT&W`'s stored profile: a Hayes modem's non-volatile settings sidecar,
//! kept with the machines' other batteries (see [`crate::csynth`]'s NVRAM
//! for the same pattern applied to a soundfont engine's memory).
//!
//! Hayes semantics: `AT&W` writes the modem's currently active settings
//! here; `ATZ` (a soft reset) reloads them, replacing whatever `AT&F`
//! (factory defaults) would otherwise leave in force. An explicit `[serial]`
//! config key always wins over a stored value at power-on -- a config file
//! is what a person wrote down on purpose, a stored profile is only what a
//! previous session happened to leave behind.

use std::path::{Path, PathBuf};

/// Whether the profile is read and written this run. Off until a frontend
/// asks for it (see [`crate::csynth::set_persistence`]'s twin), so tests and
/// library users get a modem that remembers nothing between runs, which is
/// also what `--factory` asks for.
static PERSIST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_persistence(on: bool) {
    PERSIST.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The stored profile's file, with the machines' other batteries.
fn state_path() -> PathBuf {
    crate::paths::modem_profile_file()
}

/// The modem's non-volatile settings, as `AT&W` leaves them and `ATZ`
/// restores them. Every field mirrors one of [`super::Settings`]'s or the
/// modem's other persisted knobs; kept as a separate, `serde`-friendly
/// struct rather than reusing `Settings` directly, since not everything
/// `Settings` holds is meant to survive a power cycle (nothing here is
/// in-call state).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ModemProfile {
    /// ATEn: echo command-mode bytes back to the guest.
    pub(crate) echo: bool,
    /// ATVn: verbose vs numeric result codes.
    pub(crate) verbose: bool,
    /// ATQn: suppress all result codes.
    pub(crate) quiet: bool,
    /// S0: rings before auto-answer (0 = auto-answer off).
    pub(crate) s0: u8,
    /// S2: the `+++` escape character.
    pub(crate) s2: u8,
    /// S3: the command-line terminator.
    pub(crate) s3: u8,
    /// S4: the response linefeed character.
    pub(crate) s4: u8,
    /// S9: carrier-detect response time, in tenths of a second.
    pub(crate) s9: u8,
    /// S12: escape guard time, in fiftieths of a second.
    pub(crate) s12: u8,
    /// AT&Cn: whether /CD reports asserted regardless of carrier state.
    pub(crate) dcd_always: bool,
    /// AT&Dn: what a DTR true->false transition does.
    pub(crate) dtr_action: u8,
    /// AT*T1/AT*T0: telnet NVT translation on by default.
    pub(crate) telnet: bool,
    /// The TCP port a `listen`-configured modem answers on, when the
    /// profile itself (rather than `[serial] listen`) is what set it.
    pub(crate) listen_port: Option<u16>,
    /// The port ATD appends to a bare host with no `:port` of its own.
    pub(crate) default_port: u16,
}

impl ModemProfile {
    /// Load the stored profile: `None` when persistence is off, the file is
    /// missing (an ordinary first run, not logged), or it fails to read or
    /// parse (logged, since settings that were meant to come back have not
    /// -- the same distinction [`crate::csynth::CsynthDevice::open`] draws).
    pub(crate) fn load() -> Option<Self> {
        if !PERSIST.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        Self::load_from(&state_path())
    }

    /// The read half of [`Self::load`], split out so a test can point it at
    /// a throwaway directory instead of the process-global, `PERSIST`-gated
    /// battery path -- the same reason [`crate::csynth`]'s NVRAM tests stop
    /// short of exercising real save/load (see this module's own test doc
    /// comment).
    fn load_from(path: &Path) -> Option<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(profile) => Some(profile),
                Err(e) => {
                    log::warn!("modem: reading {}: {e}", path.display());
                    None
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                log::warn!("modem: reading {}: {e}", path.display());
                None
            }
        }
    }

    /// Write the profile out. No-op when persistence is off; a write
    /// failure is logged, not propagated -- `AT&W` on real hardware has no
    /// way to fail either, beyond a dead battery it cannot detect.
    pub(crate) fn save(&self) {
        if !PERSIST.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        self.save_to(&state_path());
    }

    /// The write half of [`Self::save`], split out for the same
    /// temp-dir-testability reason as [`Self::load_from`].
    fn save_to(&self, path: &Path) {
        let text = match toml::to_string(self) {
            Ok(text) => text,
            Err(e) => {
                log::warn!("modem: encoding profile: {e}");
                return;
            }
        };
        if let Err(e) = crate::paths::ensure_parent(path).and_then(|()| std::fs::write(path, text))
        {
            log::warn!("modem: writing {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// File I/O depends on the process-global `PERSIST` flag and
    /// [`crate::paths`]'s process-global adopted configuration, neither of
    /// which a unit test can sandbox per-test (see
    /// `src/csynth/mod.rs`, which leaves the same ground untested for the
    /// same reason). What is safe to pin here is the serde round trip: the
    /// TOML shape `AT&W`/`ATZ` actually exchange.
    #[test]
    fn profile_round_trips_through_toml() {
        let profile = ModemProfile {
            echo: false,
            verbose: true,
            quiet: false,
            s0: 1,
            s2: b'+',
            s3: b'\r',
            s4: b'\n',
            s9: 6,
            s12: 50,
            dcd_always: true,
            dtr_action: 3,
            telnet: true,
            listen_port: Some(6400),
            default_port: 23,
        };
        let text = toml::to_string(&profile).unwrap();
        let reloaded: ModemProfile = toml::from_str(&text).unwrap();
        assert_eq!(reloaded, profile);
    }

    #[test]
    fn listen_port_absent_round_trips_as_none() {
        let profile = ModemProfile {
            echo: true,
            verbose: true,
            quiet: false,
            s0: 0,
            s2: b'+',
            s3: b'\r',
            s4: b'\n',
            s9: 6,
            s12: 50,
            dcd_always: false,
            dtr_action: 2,
            telnet: false,
            listen_port: None,
            default_port: 23,
        };
        let text = toml::to_string(&profile).unwrap();
        let reloaded: ModemProfile = toml::from_str(&text).unwrap();
        assert_eq!(reloaded.listen_port, None);
        assert_eq!(reloaded, profile);
    }

    /// `load`/`save` are no-ops while `PERSIST` is off, which is the state
    /// every other test in this binary leaves it in (nothing in this crate
    /// turns modem persistence on): `load` returns `None` without touching
    /// the filesystem and `save` writes nothing. That is still worth
    /// pinning -- a library user or a test harness gets a modem that
    /// remembers nothing by default, the same promise `--factory` makes.
    #[test]
    fn load_and_save_are_noops_while_persistence_is_off() {
        assert!(ModemProfile::load().is_none());
        let profile = ModemProfile {
            echo: true,
            verbose: true,
            quiet: false,
            s0: 0,
            s2: b'+',
            s3: b'\r',
            s4: b'\n',
            s9: 6,
            s12: 50,
            dcd_always: false,
            dtr_action: 2,
            telnet: false,
            listen_port: None,
            default_port: 23,
        };
        profile.save();
    }

    /// A throwaway directory under the OS temp path, unique per test run
    /// (the process id keeps parallel `cargo test` runs of this file from
    /// colliding with each other or with a stale directory from a previous
    /// run).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "copperline-modem-profile-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// [`ModemProfile::save_to`]/[`ModemProfile::load_from`] round trip
    /// through a real file in a throwaway directory -- unlike `load`/`save`,
    /// these take an explicit path rather than consulting the process-global
    /// `PERSIST` flag and [`crate::paths::modem_profile_file`], so the test
    /// touches nothing outside its own temp directory.
    #[test]
    fn save_to_and_load_from_round_trip_through_a_real_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("modem.profile");
        let profile = ModemProfile {
            echo: false,
            verbose: false,
            quiet: true,
            s0: 3,
            s2: b'+',
            s3: b'\r',
            s4: b'\n',
            s9: 5,
            s12: 20,
            dcd_always: true,
            dtr_action: 0,
            telnet: true,
            listen_port: Some(6400),
            default_port: 1541,
        };
        profile.save_to(&path);
        assert!(path.is_file());
        let reloaded = ModemProfile::load_from(&path).expect("sidecar file should parse back");
        assert_eq!(reloaded, profile);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing sidecar file is an ordinary first run, not an error: `None`
    /// with nothing logged as a failure.
    #[test]
    fn load_from_missing_file_is_none() {
        let dir = temp_dir("missing");
        let path = dir.join("does-not-exist.profile");
        assert!(ModemProfile::load_from(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `save_to` a second time overwrites the first, exactly as `AT&W`
    /// issued twice in a session should: the sidecar reflects the last
    /// settings written, not every one that was ever written.
    #[test]
    fn save_to_overwrites_a_previous_write() {
        let dir = temp_dir("overwrite");
        let path = dir.join("modem.profile");
        let mut profile = ModemProfile {
            echo: true,
            verbose: true,
            quiet: false,
            s0: 0,
            s2: b'+',
            s3: b'\r',
            s4: b'\n',
            s9: 0,
            s12: 50,
            dcd_always: false,
            dtr_action: 2,
            telnet: false,
            listen_port: None,
            default_port: 23,
        };
        profile.save_to(&path);
        profile.s0 = 2;
        profile.telnet = true;
        profile.save_to(&path);
        let reloaded = ModemProfile::load_from(&path).unwrap();
        assert_eq!(reloaded.s0, 2);
        assert!(reloaded.telnet);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
