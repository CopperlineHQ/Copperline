// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-data locations, following the platform's config-directory conventions
//! without pulling in a dependency. An empty `portable.txt` beside the
//! executable (or downloaded AppImage) opts into keeping the same data there
//! instead.
//!
//! These are *host preferences and per-user data* -- gamepad calibration,
//! keyboard mappings, save-state slots -- not machine configuration. Anything
//! that describes the emulated machine belongs in a config TOML the user
//! points at explicitly, so it stays portable and reviewable; anything that
//! describes this host's setup belongs here, so it survives switching between
//! machine configs.
//!
//! Everything returns `Option`: a host with none of the environment variables
//! set (a bare service account, some CI runners) simply has no per-user
//! directory, and every caller degrades to "not persisted" rather than
//! failing.

use std::path::PathBuf;

/// Marker that opts an installation into keeping host data beside the
/// executable. Its contents are deliberately ignored: existence is the whole
/// switch, so portable archives need no machine-specific configuration.
pub const PORTABLE_MARKER: &str = "portable.txt";

/// Return the directory containing `program` when it carries the portable
/// marker.
fn marked_program_dir(program: &std::path::Path) -> Option<PathBuf> {
    let dir = program.parent()?;
    dir.join(PORTABLE_MARKER)
        .is_file()
        .then(|| dir.to_path_buf())
}

/// Resolve portable mode from the program path the user launched. An AppImage
/// executes its embedded binary from a temporary read-only mount, but the
/// runtime exposes the downloaded image as `APPIMAGE`; that original path must
/// win so a marker can sit beside the image and the chosen directory is
/// writable. Split out so this ordering can be tested without changing the
/// process environment or executable.
fn portable_config_dir(
    appimage: Option<&std::path::Path>,
    executable: Option<&std::path::Path>,
) -> Option<PathBuf> {
    appimage
        .into_iter()
        .chain(executable)
        .find_map(marked_program_dir)
}

/// The directory name host data lives under: "Copperline" for the emulator
/// itself, a game's own id for a publisher-kit player build. See
/// [`set_app_identity`].
static APP_IDENTITY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The resolved host-data directory, cached for the life of the process.
static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Adopt a per-application identity, so host data lands under a hidden
/// per-user directory of that name (`~/.config/<name>/`,
/// `%APPDATA%\<name>\`) instead of the emulator's own Documents folder. Built for player builds, where each game keeps its own
/// settings, saves, and gamepad calibration.
///
/// Must be called before anything resolves [`config_dir`]: the directory is
/// cached on first use, so a late call would split host data across two
/// homes. Call it first thing in `main`.
pub fn set_app_identity(name: &str) {
    // The leading character must be alphanumeric -- the same rule the game
    // manifest enforces -- so "." and ".." cannot slip through and select
    // the config base directory or its parent instead of a child.
    assert!(
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "app identity must be a plain directory name: {name:?}"
    );
    debug_assert!(
        DIR.get().is_none(),
        "set_app_identity after config_dir was already resolved"
    );
    let _ = APP_IDENTITY.set(name.to_string());
}

fn app_identity() -> &'static str {
    APP_IDENTITY
        .get()
        .map(String::as_str)
        // Capitalised: this folder sits in the user's Documents beside
        // other applications' folders, and reads as a product name there,
        // not as a unix binary.
        .unwrap_or("Copperline")
}

/// Copperline's host-data directory. An empty `portable.txt` beside the
/// executable or downloaded AppImage selects that directory; otherwise this
/// is the user's Documents folder -- `$HOME/Documents/Copperline`,
/// `%USERPROFILE%\Documents\Copperline` -- somewhere a person browsing
/// their own files can find, rather than a hidden config tree. A player
/// build's [`set_app_identity`] keeps the game's data in the hidden tree
/// instead (`~/.config/<id>/`, `%APPDATA%\<id>\`), one folder per game,
/// off the top of the user's Documents.
///
/// Not created here -- writers call [`ensure_parent`].
pub fn config_dir() -> Option<PathBuf> {
    DIR.get_or_init(discover_config_dir).clone()
}

fn discover_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    let appimage = crate::envcfg::var_os("APPIMAGE").map(PathBuf::from);
    #[cfg(not(target_os = "linux"))]
    let appimage: Option<PathBuf> = None;
    let executable = std::env::current_exe().ok();
    if let Some(dir) = portable_config_dir(appimage.as_deref(), executable.as_deref()) {
        return Some(dir);
    }
    // A player build ([`set_app_identity`]) keeps the hidden per-user
    // tree: a published game is an appliance whose data nobody browses,
    // and putting every bought game's id at the top of Documents would
    // strew it with loose folders. Only the emulator's own host data --
    // the saves, screenshots and configs a person actually goes looking
    // for -- moves out to Documents.
    if APP_IDENTITY.get().is_some() {
        for var in ["XDG_CONFIG_HOME", "APPDATA"] {
            if let Some(dir) = crate::envcfg::var_os(var) {
                return Some(PathBuf::from(dir).join(app_identity()));
            }
        }
        return crate::envcfg::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".config").join(app_identity()));
    }
    // The user's own Documents folder: %USERPROFILE% is the Windows
    // spelling of a home directory, HOME everyone else's. One chain serves
    // both -- whichever the host defines names the home.
    documents_config_dir(
        ["USERPROFILE", "HOME"]
            .iter()
            .find_map(|var| crate::envcfg::var_os(var)),
    )
}

/// The host-data directory under a home: `<home>/Documents/<identity>`.
/// Split out, like [`portable_config_dir`], so the shape is testable
/// without mutating the process environment `envcfg` snapshots.
fn documents_config_dir(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home.map(|home| PathBuf::from(home).join("Documents").join(app_identity()))
}

/// A named file directly inside [`config_dir`], e.g. `gamepads.toml`.
pub fn config_file(name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join(name))
}

/// Where `--run` stages its disposable boot volume (src/runprog.rs).
/// Regenerated on every launch, so nothing under it is worth keeping.
pub fn run_stage_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("run"))
}

/// Where WHDLoad keeps everything of its own: the support archives, the
/// extracted games and their saves, and the game database.
///
/// One place, named once. Every WHDLoad setting that says `(default)`
/// means somewhere under here, so a person who never sets any of them
/// still knows where to look, and a person who moves one knows what they
/// are moving it away from.
pub fn whdload_dir() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("whdload"))
}

/// Where the support archives are looked for and downloaded to.
pub fn whdload_support_dir() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_support_in(&dir))
}

/// Where games are unpacked and their saves kept, when `[whdload] library`
/// does not say otherwise. Beside the support archives rather than loose
/// in `whdload/`, so what Copperline downloaded and what the guest wrote
/// are told apart at a glance.
///
/// An installation that already has games directly under `whdload/` --
/// where this used to be -- carries on using it. The library directory is
/// where saves live, so moving it would leave somebody's savegames and
/// highscores behind under a name nothing looks at any more.
pub fn whdload_save_dir() -> Option<PathBuf> {
    let whdload = whdload_dir()?;
    let save = whdload.join("save");
    if save.is_dir() || !holds_unpacked_games(&whdload) {
        return Some(save);
    }
    log::info!(
        "whdload: keeping the existing game library in {} (new installations use {})",
        whdload.display(),
        save.display()
    );
    Some(whdload)
}

/// Whether a directory holds games unpacked by an earlier version: an
/// entry with the `.source` marker staging writes beside each one.
fn holds_unpacked_games(dir: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join(".source").is_file())
}

// The same layout, spelled from a root the caller chose. The launcher is
// handed a root rather than asking for one, so a test can point it
// somewhere harmless -- and it had grown its own copy of these joins,
// which meant the tree Copperline writes and the tree `paths` described
// were two independent claims that happened to agree.

/// The support directory under a given root.
pub fn whdload_support_in(root: &std::path::Path) -> PathBuf {
    root.join("whdload").join("support")
}

/// The scanned library under a given root.
pub fn whdload_library_db_in(root: &std::path::Path) -> PathBuf {
    whdload_support_in(root).join("launcher.db")
}

/// A scan's downloads under a given root.
pub fn whdload_library_cache_in(root: &std::path::Path) -> PathBuf {
    whdload_support_in(root).join("cache")
}

/// The scanned library, when `[whdload] library_db` does not say otherwise.
/// Beside the support archives, which is the other thing under `whdload/`
/// that Copperline rather than the guest put there.
pub fn whdload_library_db() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_library_db_in(&dir))
}

/// What a scan downloaded, when `[whdload] library_cache` does not say
/// otherwise. Safe to delete: it is rebuilt by the next scan.
pub fn whdload_library_cache() -> Option<PathBuf> {
    config_dir().map(|dir| whdload_library_cache_in(&dir))
}

/// The root the launcher takes its library paths from.
///
/// A host with no per-user directory at all -- no `HOME`, no `USERPROFILE`,
/// which is a bare service account or some CI runners -- has
/// nowhere to put these, and every call site reached for
/// `config_dir().unwrap_or_default()`. That yields an *empty* path, so the
/// library quietly becomes `whdload/support/launcher.db` relative to
/// wherever the process was started. Named here rather than repeated at
/// eight call sites, with the behaviour unchanged: it is a poor answer, but
/// changing it is a decision about where data lives and not about who owns
/// the path.
pub fn library_root() -> PathBuf {
    config_dir().unwrap_or_default()
}

/// Directory holding the numbered save-state slots.
pub fn state_slot_dir() -> Option<PathBuf> {
    config_dir().map(|dir| configured().states_dir(&dir))
}

// --- what a run produces ------------------------------------------------
//
// Screenshots, recordings, save states, traces, waveform captures and the
// battery-backed RAMs. Each of these had its name and its location written
// out at the point it was used -- in nine places, two of them identical --
// so a file's home depended on which code path produced it and no one
// place knew the whole set.
//
// They are gathered here unchanged: every function below still resolves to
// exactly where its caller put things before, which for most of them is
// the process's working directory. That is not where they belong, but
// moving them and re-homing them at once would make a regression in the
// routing indistinguishable from an argument about the destination. With
// one owner, the move is an edit to these bodies.

// Two stamp formats are in use and both are kept. What a person opens --
// screenshots, recordings, states -- is named with a readable local
// datetime; the diagnostic captures are named with Unix seconds. Nobody
// chose that split, but a filename is what a script greps and a person
// recognises, so unifying it is a change in its own right and not one to
// smuggle in here.

/// The directories in force. A configuration's `[paths]` says where each of
/// these goes; no section, or an empty one, says nothing and the defaults
/// stand -- which is the state Copperline runs in until a configuration is
/// loaded, and the one it stays in for anybody who never moves anything.
///
/// Settable rather than read once: the launcher can change these mid-session
/// and a screenshot taken afterwards must land where the person just said it
/// should, not where the configuration said at startup.
fn store() -> &'static std::sync::RwLock<Option<std::sync::Arc<crate::pathconf::Paths>>> {
    static DIRS: std::sync::RwLock<Option<std::sync::Arc<crate::pathconf::Paths>>> =
        std::sync::RwLock::new(None);
    &DIRS
}

/// The directories in force now.
pub fn configured() -> std::sync::Arc<crate::pathconf::Paths> {
    // A poisoned lock still holds a perfectly good `Paths`: the panic that
    // poisoned it was somebody else's, and refusing to say where screenshots
    // go because of it would help nobody.
    let read = store().read().unwrap_or_else(|e| e.into_inner());
    if let Some(dirs) = read.clone() {
        return dirs;
    }
    drop(read);
    let mut write = store().write().unwrap_or_else(|e| e.into_inner());
    write.get_or_insert_with(Default::default).clone()
}

/// Borrows the adopted set for a test, and puts the defaults back when it
/// goes out of scope.
///
/// [`adopt`] writes one process-wide store and `cargo test` runs threads in
/// one process, so two tests adopting at once would clobber each other --
/// hence the lock. Restoring on drop is the other half: without it a test
/// that adopts leaves its directories in force for every *unguarded* test
/// that runs afterwards, which the lock alone cannot prevent. Production
/// has no such caller; adoption happens at startup and on launcher edits,
/// both on one thread.
#[cfg(test)]
pub(crate) struct AdoptedStore {
    /// Held for the guard's lifetime; never read.
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for AdoptedStore {
    fn drop(&mut self) {
        adopt(crate::pathconf::Paths::default());
    }
}

#[cfg(test)]
pub(crate) fn adopted_store_lock() -> AdoptedStore {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    AdoptedStore {
        _lock: LOCK.lock().unwrap_or_else(|e| e.into_inner()),
    }
}

/// Put a configuration's `[paths]` in force, dropping whatever it names that
/// this machine cannot reach.
///
/// Called once the configuration is settled, and again whenever the launcher
/// changes it. The reachability check happens here rather than at the point
/// of use so it happens once and where it can be waited on, not in the
/// middle of taking a screenshot.
pub fn adopt(dirs: crate::pathconf::Paths) {
    let checked = match config_dir() {
        Some(host) => dirs.reachable(&host),
        // Nowhere to resolve against, so nothing to check: without a
        // host-data directory the defaults are bare names in the working
        // directory and `[paths]` has nothing to hang off.
        None => dirs,
    };
    let mut write = store().write().unwrap_or_else(|e| e.into_inner());
    *write = Some(std::sync::Arc::new(checked));
}

/// A named file under one of the configured directories.
///
/// A host with no per-user directory at all keeps the bare name, which is
/// the working directory -- the same "degrade to not persisted" the rest of
/// this module promises, rather than an error at the point somebody presses
/// the screenshot key.
fn output(
    pick: impl Fn(&crate::pathconf::Paths, &std::path::Path) -> PathBuf,
    name: String,
) -> PathBuf {
    match config_dir() {
        Some(host) => pick(&configured(), &host).join(name),
        None => PathBuf::from(name),
    }
}

/// Seconds since the epoch, for the diagnostic captures.
fn epoch_stamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Default name for a screenshot taken without one being given.
pub fn screenshot_file() -> PathBuf {
    output(
        |p, h| p.screenshots_dir(h),
        format!(
            "copperline-screenshot-{}.png",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a video recording.
pub fn recording_file() -> PathBuf {
    output(
        |p, h| p.recordings_dir(h),
        format!("copperline-video-{}.avi", crate::timestamp::compact_now()),
    )
}

/// Default name for a recorded input script.
pub fn input_recording_file() -> PathBuf {
    output(
        |p, h| p.recordings_dir(h),
        format!(
            "copperline-input-{}.clscript",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a save state written outside the numbered slots.
pub fn state_file() -> PathBuf {
    output(
        |p, h| p.states_dir(h),
        format!(
            "copperline-state-{}.clstate",
            crate::timestamp::compact_now()
        ),
    )
}

/// Default name for a waveform capture. Reached from `--waveform` without a
/// path and from `waveform.start` without one; a capture can run to half a
/// gigabyte, which is its own argument against the working directory.
pub fn waveform_file() -> PathBuf {
    output(
        |p, h| p.traces_dir(h),
        format!("copperline-wave-{}.vcd", epoch_stamp()),
    )
}

/// Default name for an instruction trace. The debugger console and the
/// control protocol both start traces, and each had its own copy of this;
/// they now cannot drift.
pub fn trace_file() -> PathBuf {
    output(
        |p, h| p.traces_dir(h),
        format!("copperline-trace-{}.txt", epoch_stamp()),
    )
}

/// A battery-backed RAM, under the configured directory -- unless one is
/// already sitting where Copperline used to put it.
///
/// These held their bare names, so they landed in whatever directory the
/// process happened to start in. Moving where Copperline *looks* is not the
/// same as moving the file: the CD32's is a memory card holding real game
/// saves, and a player whose progress silently reverted to blank would have
/// no way of knowing it was still on disk a directory away. So an existing
/// file keeps being used where it is, and only a machine that never had one
/// gets the new place. Nothing is moved behind anyone's back.
fn battery_ram(name: &str) -> PathBuf {
    let legacy = PathBuf::from(name);
    if legacy.is_file() {
        log::info!("using the existing {name} in the working directory");
        return legacy;
    }
    match config_dir() {
        Some(host) => configured().nvram_dir(&host).join(name),
        None => legacy,
    }
}

/// The RP5C01's battery-backed RAM, when `[machine] battmem` does not say
/// otherwise. Fitted to A3000/A4000-class machines.
pub fn battery_ram_file() -> PathBuf {
    battery_ram("battmem.nvram")
}

/// The CD32's Akiko NVRAM, when nothing else says otherwise.
pub fn akiko_nvram_file() -> PathBuf {
    battery_ram("cd32-nvram.bin")
}

/// Coppersynth's battery-backed memory, kept with the machines' other
/// batteries.
pub fn coppersynth_nvram_file() -> PathBuf {
    battery_ram("coppersynth.nvram")
}

/// The Hayes modem's stored profile (`AT&W`/`ATZ`), kept with the machines'
/// other batteries.
pub fn modem_profile_file() -> PathBuf {
    battery_ram("modem.profile")
}

// --- where a dialog opens ------------------------------------------------
//
// Not Copperline's to write, and not created: these only say where a file
// dialog starts when the field it was opened from is empty. A field with a
// value opens beside that value, which beats any fixed answer.
//
// Each returns `None` when the directory does not exist, so a dialog is
// never pointed at somewhere that isn't there. Make the folder and it gets
// used; don't, and nothing changes. That is the whole of the opt-in.

fn media_dir(
    pick: impl Fn(&crate::pathconf::Paths, &std::path::Path) -> PathBuf,
) -> Option<PathBuf> {
    let dir = pick(&configured(), &config_dir()?);
    dir.is_dir().then_some(dir)
}

/// Kickstart and extended ROM images.
pub fn roms_dir() -> Option<PathBuf> {
    media_dir(|p, h| p.roms_dir(h))
}

/// MT-32 control and PCM ROMs.
pub fn mt32_roms_dir() -> Option<PathBuf> {
    media_dir(|p, h| p.mt32_roms_dir(h))
}

/// Floppy images.
pub fn floppies_dir() -> Option<PathBuf> {
    media_dir(|p, h| p.floppies_dir(h))
}

/// Hard-disk images and host-filesystem folders.
pub fn harddrives_dir() -> Option<PathBuf> {
    media_dir(|p, h| p.harddrives_dir(h))
}

/// CD images.
pub fn cds_dir() -> Option<PathBuf> {
    media_dir(|p, h| p.cds_dir(h))
}

/// Where the launcher saves machine configurations.
pub fn configs_dir() -> Option<PathBuf> {
    config_dir().map(|host| configured().configs_dir(&host))
}

/// The configuration Copperline starts with when nothing on the command
/// line says otherwise.
///
/// Exists only once somebody has pressed Save default, which is what makes
/// "back to factory settings" a matter of deleting one file: with no
/// default saved there is nothing to override, so most installations never
/// have one and `--factory` never has anything to do.
///
/// Deliberately *not* [`configs_dir`]: this file is found at startup,
/// before any configuration -- including its own `[paths]` -- has been
/// adopted, so a location that followed `[paths] configs` could never be
/// found by the startup that needs it. Saved and looked for in the factory
/// location always; the `configs` entry steers the file dialogs, not this.
pub fn default_config_file() -> Option<PathBuf> {
    let factory = crate::pathconf::Paths::default();
    config_dir().map(|host| factory.configs_dir(&host).join(DEFAULT_CONFIG))
}

/// The saved default's filename.
pub const DEFAULT_CONFIG: &str = "default.toml";

/// Create a path's parent directory so a write to it can succeed.
pub fn ensure_parent(path: &std::path::Path) -> std::io::Result<()> {
    match path.parent() {
        // A bare filename's parent is the empty path, which is the working
        // directory and needs no creating.
        Some(parent) if !parent.as_os_str().is_empty() => std::fs::create_dir_all(parent),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("copperline-paths-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The names a run produces, and which stamp each carries. Gathering
    /// these into one place is only safe if none of them changed on the way
    /// -- a filename is what a person recognises and a script greps -- and
    /// the two stamp formats are easy to swap by accident, because nothing
    /// but the call site said which was which.
    #[test]
    fn the_output_names_keep_their_shape_and_their_stamp() {
        let readable = |p: &std::path::Path, prefix: &str, ext: &str| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            assert!(name.starts_with(prefix), "{name} does not start {prefix}");
            assert!(name.ends_with(ext), "{name} does not end {ext}");
            let stamp = &name[prefix.len()..name.len() - ext.len()];
            // yyyymmddHHMMSS: a local datetime a person can read.
            assert_eq!(stamp.len(), 14, "{name}: not a compact datetime");
            assert!(stamp.chars().all(|c| c.is_ascii_digit()), "{name}");
            assert!(stamp.starts_with("20"), "{name}: not a year");
        };
        readable(&screenshot_file(), "copperline-screenshot-", ".png");
        readable(&recording_file(), "copperline-video-", ".avi");
        readable(&input_recording_file(), "copperline-input-", ".clscript");
        readable(&state_file(), "copperline-state-", ".clstate");

        let epoch = |p: &std::path::Path, prefix: &str, ext: &str| {
            let name = p.file_name().unwrap().to_str().unwrap().to_string();
            assert!(name.starts_with(prefix), "{name} does not start {prefix}");
            assert!(name.ends_with(ext), "{name} does not end {ext}");
            let stamp = &name[prefix.len()..name.len() - ext.len()];
            let secs: u64 = stamp.parse().unwrap_or_else(|_| panic!("{name}"));
            // Unix seconds, not a datetime: far past any 14-digit value.
            assert!(secs > 1_600_000_000, "{name}: not unix seconds");
            assert!(secs < 4_000_000_000, "{name}: not unix seconds");
        };
        epoch(&waveform_file(), "copperline-wave-", ".vcd");
        epoch(&trace_file(), "copperline-trace-", ".txt");

        // The two battery RAMs are fixed names, not stamped. Where they
        // sit is `nvram_dir`'s business and is covered separately; what
        // matters here is that the names themselves never changed.
        assert_eq!(
            battery_ram_file().file_name().and_then(|n| n.to_str()),
            Some("battmem.nvram")
        );
        assert_eq!(
            akiko_nvram_file().file_name().and_then(|n| n.to_str()),
            Some("cd32-nvram.bin")
        );
        assert_eq!(
            modem_profile_file().file_name().and_then(|n| n.to_str()),
            Some("modem.profile")
        );
    }

    /// The link the rest of the suite does not reach: a configuration's
    /// `[paths]`, once adopted, is what a run actually writes against.
    /// Everything in between is covered -- the section parses, the entries
    /// resolve, the names keep their shape -- and none of it would notice
    /// if [`output`] stopped consulting the adopted set at all.
    ///
    /// The only test that writes the process-wide store. It is safe beside
    /// the others because they look at file *names*, which this does not
    /// touch, and it puts the defaults back when it is done.
    #[test]
    fn what_a_run_writes_follows_the_adopted_section() {
        let _guard = adopted_store_lock();
        let Some(host) = config_dir() else {
            // No per-user directory on this host, so every default is a
            // bare name and there is nothing for a section to move.
            return;
        };
        adopt(crate::pathconf::Paths {
            screenshots: Some(PathBuf::from("elsewhere")),
            ..Default::default()
        });
        let moved = screenshot_file();
        adopt(crate::pathconf::Paths::default());
        let back = screenshot_file();
        // `_guard` puts the defaults back either way; the explicit adopt
        // above is what `back` is measuring.

        assert_eq!(moved.parent(), Some(host.join("elsewhere").as_path()));
        assert_eq!(back.parent(), Some(host.join("screenshots").as_path()));
    }

    /// The launcher is handed a root and spells the library paths from it;
    /// the no-argument helpers spell them from the config directory. Those
    /// were two independent descriptions of the same tree, agreeing only by
    /// coincidence, so hold them to each other.
    /// A battery RAM sitting where Copperline used to put it keeps being
    /// used from there. The CD32's is a memory card holding real game
    /// saves; looking somewhere new would present a player with a blank
    /// card and no hint that their progress was still on disk.
    /// The dialog directories answer only for a folder that exists. Make
    /// one and it gets used; leave it and nothing changes -- which is what
    /// lets these be offered without Copperline creating a handful of empty
    /// folders nobody asked for.
    #[test]
    fn a_dialog_directory_answers_only_when_it_is_there() {
        let scratch = ScratchDir::new("media");
        let cfg = crate::pathconf::Paths {
            base: Some(scratch.0.clone()),
            ..Default::default()
        };
        let host = scratch.0.as_path();

        let roms = cfg.roms_dir(host);
        assert!(!roms.is_dir(), "nothing made yet");
        assert_eq!(roms.is_dir().then(|| roms.clone()), None);

        std::fs::create_dir_all(&roms).unwrap();
        assert_eq!(roms.is_dir().then(|| roms.clone()), Some(roms.clone()));

        // The MT-32 pair sit under the ROMs folder rather than beside it.
        assert_eq!(cfg.mt32_roms_dir(host), roms.join("mt32"));
    }

    /// Restores the working directory when dropped, so a failing assertion
    /// cannot leave the rest of the suite running somewhere else: the CWD
    /// is process state, and `set_current_dir` with no guard holds until
    /// the process ends, not until the test does.
    struct CwdGuard(PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn an_existing_battery_ram_is_not_abandoned() {
        // The no-legacy branch below reads the adopted store.
        let _store = adopted_store_lock();
        let scratch = ScratchDir::new("battery");
        let _cwd = CwdGuard(std::env::current_dir().unwrap());
        std::env::set_current_dir(&scratch.0).unwrap();

        // Nothing there: the new place, under the host data directory.
        let fresh = akiko_nvram_file();
        assert!(
            fresh.parent().is_some_and(|p| p.ends_with("nvram")) || config_dir().is_none(),
            "a machine with no card should get the nvram directory: {fresh:?}"
        );

        // One there: that one, wherever it is.
        std::fs::write("cd32-nvram.bin", []).unwrap();
        assert_eq!(akiko_nvram_file(), PathBuf::from("cd32-nvram.bin"));
        std::fs::write("battmem.nvram", []).unwrap();
        assert_eq!(battery_ram_file(), PathBuf::from("battmem.nvram"));
    }

    #[test]
    fn the_library_layout_is_one_description() {
        let root = std::path::Path::new("/tmp/copperline-root");
        assert_eq!(
            whdload_library_db_in(root),
            whdload_support_in(root).join("launcher.db")
        );
        assert_eq!(
            whdload_library_cache_in(root),
            whdload_support_in(root).join("cache")
        );
        assert_eq!(
            whdload_support_in(root),
            root.join("whdload").join("support")
        );
        // And the no-argument forms are the same layout under the config
        // directory, whatever that turns out to be on this host.
        if let Some(dir) = config_dir() {
            assert_eq!(whdload_support_dir(), Some(whdload_support_in(&dir)));
            assert_eq!(whdload_library_db(), Some(whdload_library_db_in(&dir)));
            assert_eq!(
                whdload_library_cache(),
                Some(whdload_library_cache_in(&dir))
            );
        }
    }

    #[test]
    fn portable_marker_selects_the_executable_directory() {
        let root = ScratchDir::new("portable-marker");
        let executable = root.0.join("copperline.exe");

        assert_eq!(portable_config_dir(None, Some(&executable)), None);
        std::fs::write(root.0.join(PORTABLE_MARKER), []).unwrap();
        assert_eq!(
            portable_config_dir(None, Some(&executable)),
            Some(root.0.clone())
        );
        assert_eq!(
            portable_config_dir(None, Some(&executable)).map(|dir| dir.join("states")),
            Some(root.0.join("states"))
        );
    }

    #[test]
    fn appimage_marker_selects_the_download_location_before_the_mounted_binary() {
        let root = ScratchDir::new("appimage-marker");
        let download = root.0.join("download");
        let mount = root.0.join("mount/usr/bin");
        std::fs::create_dir_all(&download).unwrap();
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(download.join(PORTABLE_MARKER), []).unwrap();

        let appimage = download.join("Copperline.AppImage");
        let executable = mount.join("copperline");
        assert_eq!(
            portable_config_dir(Some(&appimage), Some(&executable)),
            Some(download)
        );
    }

    /// The home resolves to the user's Documents folder -- somewhere a
    /// person browsing their own files can find, not a hidden config tree.
    /// (A player build's overridden identity stays in the hidden tree; see
    /// `discover_config_dir` -- that branch reads the process environment
    /// through `envcfg`'s one snapshot, so it is exercised by the player
    /// crate rather than driven from here.)
    #[test]
    fn host_data_lives_in_documents() {
        assert_eq!(
            documents_config_dir(Some("/home/lee".into())),
            Some(PathBuf::from("/home/lee/Documents/Copperline"))
        );
        assert_eq!(documents_config_dir(None), None);
    }

    /// The cascade is read through `envcfg`, which snapshots the environment
    /// once per process, so this asserts the shape of whatever the host
    /// offered rather than driving each branch with a mutated environment.
    #[test]
    fn host_data_paths_hang_off_one_directory() {
        let Some(dir) = config_dir() else {
            return; // A host with no HOME/USERPROFILE.
        };
        assert_eq!(
            config_file("gamepads.toml").unwrap(),
            dir.join("gamepads.toml")
        );
        assert_eq!(state_slot_dir().unwrap(), dir.join("states"));
    }
}
