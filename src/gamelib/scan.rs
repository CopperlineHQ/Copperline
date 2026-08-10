// SPDX-License-Identifier: GPL-3.0-or-later

//! Resolving a folder of packages into a library with metadata.
//!
//! Nothing here happens on its own. Opening the Library page lists what is
//! in the game folder and reads what the last scan resolved; it does not
//! go looking. A folder can hold thousands of packages on a slow disk, and
//! a page that scanned whenever it was opened would be a page that stalls
//! for a minute because somebody clicked the wrong tab. The two buttons say
//! when: Refresh re-reads the folder, and Scan runs this.
//!
//! A scan is four pieces of work, in order:
//!
//! 1. **The catalogue.** The snapshot in the cache directory is brought up
//!    to date from the sync cursor, which is a few pages after a first run
//!    and all of it before one. Without a signed-in session it is used as
//!    it stands -- local first, always -- and a scan with neither a session
//!    nor a snapshot is the one case that has nothing to work from.
//! 2. **Reading.** Each package is opened far enough to take the digest
//!    of its WHDLoad slave, which identifies both the game and the
//!    package itself across a rename. Only packages not read before.
//!    See [`slave_digests`].
//! 3. **Matching.** By that digest where the catalogue knows it, and by
//!    name otherwise.
//! 4. **Art.** Covers the catalogue points at and the cache has not got are
//!    fetched. This is the long part of a first scan, and the part a second
//!    one skips entirely.
//!
//! It runs on a worker thread and reports as it goes. It can be stopped at
//! any point between items -- by the button, or by the machine starting,
//! since a person who has pressed Run is done with the launcher -- and a
//! stopped scan keeps what it had resolved rather than throwing it away.
//! Nothing it meets is fatal: an unreadable folder, a corrupt snapshot, a
//! service that will not answer or answers with nonsense all end as a short
//! line on the status bar, because a launcher that panics has lost a
//! person's session over a file they have never heard of.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use super::db::{Catalogue, Known};
use super::openretro::Session;

/// What a running scan has to say for itself.
#[derive(Debug, Clone)]
pub enum Progress {
    /// Reading the game folder.
    Listing,
    /// Bringing the catalogue snapshot up to date, so far.
    Syncing { games: usize },
    /// Reading the packages, to identify them by what is inside.
    Reading { done: usize, total: usize },
    /// Looking packages up.
    Matching { done: usize, total: usize },
    /// Everything matched, before any art has been fetched. Sent as soon
    /// as it is known so the page fills in with names, years and
    /// publishers while the slow part is still running -- a scan of a few
    /// thousand games that is interrupted at 60% has still done 60% of the
    /// useful work, and there is no reason to sit on it.
    Matched { known: Vec<Known>, matched: usize },
    /// Fetching the art that is missing.
    Art { done: usize, total: usize },
    /// Finished. What it resolved came in [`Progress::Matched`], which is
    /// sent before the art whether the scan finishes or is stopped, so
    /// this only has to say how it ended.
    Done {
        /// Whether it ran to the end rather than being stopped.
        complete: bool,
        /// What the online database contributed: how many entries the sync
        /// brought in, or `None` when it was never asked because nobody is
        /// signed in. Reported because "scan complete" on its own reads as
        /// "checked with OpenRetro" whether or not anything did.
        synced: Option<usize>,
    },
    /// Gave up. The string is short enough for the status bar, which is
    /// perhaps forty characters wide before it starts clipping; whatever
    /// else is worth knowing has already gone to the log.
    Failed(String),
}

impl Progress {
    /// The line the status bar shows for it.
    pub fn message(&self) -> String {
        match self {
            Progress::Listing => "Reading the game folder...".to_string(),
            Progress::Syncing { games } => format!("Updating the game database... {games} games"),
            Progress::Reading { done, total } => format!("Reading packages... {done}/{total}"),
            Progress::Matching { done, total } => format!("Matching games... {done}/{total}"),
            Progress::Matched { matched, known } => {
                format!("Matched {matched} of {} games", known.len())
            }
            Progress::Art { done, total } => format!("Fetching cover art... {done}/{total}"),
            Progress::Done {
                complete: false, ..
            } => "Scan stopped -- kept what it had".to_string(),
            Progress::Done { synced: None, .. } => {
                "Scan complete -- not logged in, used the cached database".to_string()
            }
            Progress::Done {
                synced: Some(0), ..
            } => "Scan complete -- database already up to date".to_string(),
            Progress::Done {
                synced: Some(n), ..
            } => format!("Scan complete -- {n} database entries updated"),
            Progress::Failed(why) => why.clone(),
        }
    }

    /// Whether this is the last thing a scan will say.
    pub fn is_last(&self) -> bool {
        matches!(self, Progress::Done { .. } | Progress::Failed(_))
    }
}

/// A scan in flight.
pub struct Scan {
    rx: Receiver<Progress>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for Scan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Scan { .. }")
    }
}

impl Drop for Scan {
    /// Dropping the handle stops the worker: it checks the flag between
    /// items, sees it set, and finishes rather than carrying on fetching
    /// art for a page nobody is looking at.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Scan {
    /// Start one. `session` is what a signed-in launcher has; without it
    /// the scan works from the cached snapshot alone. `held` is the slave
    /// digest already worked out for each package, so a rescan does not
    /// open the archives it has read before.
    pub fn start(
        folder: PathBuf,
        cache: PathBuf,
        session: Option<Arc<Session>>,
        held: HashMap<String, String>,
    ) -> Scan {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        std::thread::spawn(move || {
            // Whatever goes wrong in there -- including a panic, which is a
            // bug rather than a condition, but is still not worth a lost
            // session over -- comes back as a line rather than as a crash.
            let sent = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&folder, &cache, session.as_deref(), &held, &flag, &tx)
            }));
            if let Err(_panic) = sent {
                let _ = tx.send(Progress::Failed("Scan failed unexpectedly".to_string()));
            }
        });
        Scan { rx, stop }
    }

    /// Everything said since the last look, oldest first.
    pub fn poll(&self) -> Vec<Progress> {
        self.rx.try_iter().collect()
    }

    /// Ask it to stop at the next item. It always gets to finish the one it
    /// is on, so nothing is left half written.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Where the catalogue snapshot sits inside the cache directory.
pub fn catalogue_path(cache: &Path) -> PathBuf {
    cache.join("openretro.json")
}

/// Where fetched art sits inside the cache directory.
pub fn covers_path(cache: &Path) -> PathBuf {
    cache.join("covers")
}

/// The scan itself, on the worker thread.
fn run(
    folder: &Path,
    cache: &Path,
    session: Option<&Session>,
    held: &HashMap<String, String>,
    stop: &AtomicBool,
    tx: &Sender<Progress>,
) {
    // Stopping keeps what has been resolved: a library half filled in
    // beats one not filled in at all, and the next scan carries on from a
    // cache that is further along.
    let stopped = |tx: &Sender<Progress>, known: Vec<Known>, matched: usize| {
        let _ = tx.send(Progress::Matched { known, matched });
        let _ = tx.send(Progress::Done {
            complete: false,
            synced: None,
        });
    };

    let _ = tx.send(Progress::Listing);
    if !folder.is_dir() {
        log::error!("game library: {} is not a directory", folder.display());
        let _ = tx.send(Progress::Failed("No game library there".to_string()));
        return;
    }
    let files = packages(folder);
    if files.is_empty() {
        log::error!("game library: no .lha packages under {}", folder.display());
        let _ = tx.send(Progress::Failed(
            "No packages in the game library".to_string(),
        ));
        return;
    }

    // The catalogue, local first: what is cached is used as it stands, and
    // a session brings it up to date from where it left off.
    let at = catalogue_path(cache);
    let mut catalogue = Catalogue::load(&at);
    let had = catalogue.len();
    // How many entries the online database contributed, or `None` if it
    // was never asked. A scan that says nothing about this is a scan a
    // person reasonably reads as having checked when it has not.
    let mut synced = None;
    if let Some(session) = session {
        let outcome = sync(session, &mut catalogue, stop, tx);
        synced = Some(*outcome.as_ref().unwrap_or_else(|(_, brought)| brought));
        // Whatever arrived is kept, cursor and all, whether the sync
        // finished or the network went away in the middle of it. That is
        // what makes the next scan resume rather than start again -- and
        // a connection that drops on page forty of fifty should cost the
        // last ten pages, not the first forty.
        if catalogue.len() != had {
            if let Err(e) = catalogue.save(&at) {
                log::warn!("game library: could not cache the catalogue: {e}");
            }
        }
        if let Err((why, _)) = outcome {
            // A sync that failed is not a scan that failed, as long as
            // there is something to match against.
            if catalogue.is_empty() {
                log::error!("game library: {why}, and nothing cached to match against");
                let _ = tx.send(Progress::Failed("Could not reach OpenRetro".to_string()));
                return;
            }
            log::warn!("game library: {why}; matching against what is cached");
        }
    } else if catalogue.is_empty() {
        log::error!(
            "game library: no cached game database and no OpenRetro session; \
             press Log in on the WHDLoad page and scan again"
        );
        let _ = tx.send(Progress::Failed("Not logged in to OpenRetro".to_string()));
        return;
    }
    if stop.load(Ordering::Relaxed) {
        stopped(tx, Vec::new(), 0);
        return;
    }

    // Matching, by what is inside a package before what it is called.
    let total = files.len();
    let digests = slave_digests(folder, &files, held, stop, tx);
    if stop.load(Ordering::Relaxed) {
        stopped(tx, Vec::new(), 0);
        return;
    }
    let mut known = Vec::with_capacity(total);
    let (mut matched, mut by_digest) = (0, 0);
    for (done, file) in files.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            stopped(tx, known, matched);
            return;
        }
        if done % 64 == 0 {
            let _ = tx.send(Progress::Matching { done, total });
        }
        let slave_sha1 = digests[done].clone();
        // The digest first: where it answers, the bytes of that slave are
        // that game's and there is nothing to be wrong about. The name is
        // the fallback, for a package the catalogue has no WHDLoad release
        // of -- which is most of a third of them.
        let game = slave_sha1
            .as_deref()
            .and_then(|sha1| catalogue.match_digest(sha1))
            .inspect(|_| by_digest += 1)
            .or_else(|| catalogue.match_file(file.rsplit('/').next().unwrap_or(file)))
            .cloned();
        matched += usize::from(game.is_some());
        known.push(Known {
            file: file.clone(),
            game,
            slave_sha1,
            // A scan never claims one as hand-filled; the store keeps
            // whichever entries already were.
            manual: false,
        });
    }
    log::info!(
        "game library: {matched}/{total} matched, {by_digest} of them by slave digest \
         ({} digests known)",
        catalogue.digest_count()
    );

    // The art each match wants, worked out before the result is handed
    // over -- after it, `known` belongs to the launcher.
    let covers = covers_path(cache);
    let wanted: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        known
            .iter()
            .filter_map(|k| k.game.as_ref()?.front_sha1.as_deref())
            .filter(|sha1| seen.insert(*sha1))
            .filter(|sha1| !super::cover::cover_file(&covers, sha1).exists())
            .map(str::to_string)
            .collect()
    };
    let _ = tx.send(Progress::Matched { known, matched });

    // Art. The long part of a first scan, and skipped entirely by a second:
    // only digests the cache has not got are fetched.
    if !wanted.is_empty() {
        if std::fs::create_dir_all(&covers).is_err() {
            log::warn!("game library: cannot write to {}", covers.display());
        } else {
            fetch_art(&wanted, &covers, stop, tx);
        }
    }
    let _ = tx.send(Progress::Done {
        complete: !stop.load(Ordering::Relaxed),
        synced,
    });
}

/// The digest of each package's WHDLoad slave, in the order given.
///
/// Two jobs, which is why every package is read rather than a sample.
///
/// The first is identity. A slave's bytes belong to exactly one game, so
/// where the catalogue knows that digest the match is an answer rather
/// than a guess about a file name. In practice OpenRetro's WHDLoad file
/// lists were imported in 2015 and slaves are revised with every release,
/// so a current collection matches almost none of them -- but it costs a
/// couple of seconds to find out, and the exact path lights up for nothing
/// if that data is ever refreshed.
///
/// The second is what makes it worth doing regardless: a digest is how a
/// package is recognised after it has been renamed. Metadata somebody
/// corrected by hand is filed against the package, and without this a
/// rename would strand it -- see [`Database::set_known`].
///
/// `held` supplies what a previous scan worked out, so a rescan opens only
/// the packages it has not seen. Reading one means decompressing a single
/// member out of an archive -- a few kilobytes out of tens of megabytes --
/// and the archives are independent, so they are read several at a time.
///
/// An archive that will not open, has no slave, or fails its CRC is not an
/// error: it is one package matched by name instead. A library assembled
/// over years has a few of those in it, and a scan that stopped at the
/// first would never get past it.
fn slave_digests(
    folder: &Path,
    files: &[String],
    held: &HashMap<String, String>,
    stop: &AtomicBool,
    tx: &Sender<Progress>,
) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = files.iter().map(|file| held.get(file).cloned()).collect();
    let todo: Vec<usize> = (0..files.len()).filter(|&i| out[i].is_none()).collect();
    read_digests(folder, files, &todo, &mut out, stop, tx);
    out
}

/// Read the slave digest of each package in `todo` into `out`.
fn read_digests(
    folder: &Path,
    files: &[String],
    todo: &[usize],
    out: &mut [Option<String>],
    stop: &AtomicBool,
    tx: &Sender<Progress>,
) {
    if todo.is_empty() {
        return;
    }
    let (found_tx, found_rx) = std::sync::mpsc::channel::<(usize, String)>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = todo.len();
    std::thread::scope(|scope| {
        for _ in 0..READ_WORKERS.min(total) {
            let (tx, found_tx) = (tx.clone(), found_tx.clone());
            let (next, done, todo) = (&next, &done, &todo);
            scope.spawn(move || loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let slot = next.fetch_add(1, Ordering::Relaxed);
                let Some(&at) = todo.get(slot) else { return };
                let at_path = crate::package::under(folder, &files[at]);
                if let Some(sha1) = read_slave_digest(&at_path) {
                    let _ = found_tx.send((at, sha1));
                }
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n % 25 == 0 || n == total {
                    let _ = tx.send(Progress::Reading { done: n, total });
                }
            });
        }
        // The workers' clones are what hold the channel open; this one
        // would hold it open for ever.
        drop(found_tx);
    });
    for (at, sha1) in found_rx {
        out[at] = Some(sha1);
    }
}

/// One package's slave digest, or `None` for anything that is not a
/// readable archive with a slave in it.
fn read_slave_digest(path: &Path) -> Option<String> {
    // Nothing in here is trusted: the archive is somebody's download, and
    // a decoder given a corrupt one may well panic rather than return an
    // error. That is one bad file in a library, not a scan that dies.
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::package::read_first(path, |member| {
            member
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(crate::package::is_slave_name)
        })
    }));
    match read {
        Ok(Ok(Some(data))) => Some(super::sha1::hex(&data)),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            log::debug!(
                "game library: {} has no readable slave: {e}",
                path.display()
            );
            None
        }
        Err(_) => {
            log::warn!("game library: {} could not be read at all", path.display());
            None
        }
    }
}

/// How many archives are opened at once. They are read from disk and
/// decompressed, so this is bounded by cores rather than by politeness.
const READ_WORKERS: usize = 6;

/// How many covers are fetched at once.
///
/// A library of a couple of thousand games wants nearly as many pictures,
/// and one at a time over a network round trip is the difference between a
/// scan that finishes while you wait and one you give up on. Six is enough
/// to keep the link busy and few enough to be a polite number of
/// connections to hold open against somebody else's server.
const ART_WORKERS: usize = 6;

/// Fetch the missing art, several at a time.
///
/// Each worker keeps its own agent, so the connection is reused across the
/// hundreds of requests it makes rather than handshaked afresh for each.
/// One that will not come is one game without a picture, not a scan that
/// failed -- and each picture is written as it lands, so a scan stopped
/// half way leaves half the art on disk rather than none of it.
fn fetch_art(wanted: &[String], covers: &Path, stop: &AtomicBool, tx: &Sender<Progress>) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let total = wanted.len();
    std::thread::scope(|scope| {
        for _ in 0..ART_WORKERS.min(total) {
            let tx = tx.clone();
            let (next, done) = (&next, &done);
            scope.spawn(move || {
                let agent = super::openretro::covers_agent();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(sha1) = wanted.get(at) else { return };
                    match super::openretro::cover_with(&agent, sha1, super::openretro::COVER_PIXELS)
                    {
                        Ok(png) => write_cover(covers, sha1, &png),
                        Err(e) => log::debug!("game library: no art for {sha1}: {e}"),
                    }
                    // Reported in tens: a message a picture is a message a
                    // frame, and the number on screen cannot be read that
                    // fast anyway.
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n % 10 == 0 || n == total {
                        let _ = tx.send(Progress::Art { done: n, total });
                    }
                }
            });
        }
    });
}

/// Write one cover, through a temporary so a download cut off half way
/// does not leave a broken file the next scan would take for done.
fn write_cover(covers: &Path, sha1: &str, png: &[u8]) {
    let at = super::cover::cover_file(covers, sha1);
    let temp = at.with_extension("png.partial");
    if std::fs::write(&temp, png).is_ok() && std::fs::rename(&temp, &at).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// Bring the snapshot up to date, a page at a time.
fn sync(
    session: &Session,
    catalogue: &mut Catalogue,
    stop: &AtomicBool,
    tx: &Sender<Progress>,
) -> Result<usize, (String, usize)> {
    // The cursor is what makes this a check rather than a download: the
    // service is asked for what has changed since last time, so a library
    // already up to date costs one empty page.
    let mut brought = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(brought);
        }
        let page = match session.games_since(catalogue.cursor) {
            Ok(page) => page,
            Err(e) => return Err((format!("Could not reach OpenRetro: {e}"), brought)),
        };
        if page.is_empty() {
            return Ok(brought);
        }
        brought += catalogue.apply(&page);
        let _ = tx.send(Progress::Syncing {
            games: catalogue.len(),
        });
    }
}

/// Every package in a folder, as paths relative to it.
///
/// Relative rather than bare names because a collection filed by letter
/// has two `Zool_v1.0.lha` in it as often as not, and the store has to be
/// able to tell them apart.
pub fn packages(folder: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(folder, folder, 0, &mut 0, &mut out);
    out
}

/// How deep the search goes, and how many directories it opens before
/// giving up. The same limits the list itself uses, and for the same
/// reasons: a library pointed at a home directory should stop rather than
/// read somebody's whole disk.
const MAX_DEPTH: usize = 6;
const MAX_DIRS: usize = 4000;

fn walk(root: &Path, dir: &Path, depth: usize, walked: &mut usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || *walked >= MAX_DIRS {
        return;
    }
    *walked += 1;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Written with forward slashes whatever the host uses, so a store
    // written on one machine still matches on another.
    let relative = |path: &Path| -> Option<String> {
        let under = path.strip_prefix(root).ok()?;
        let parts: Vec<String> = under
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    };
    for entry in entries.flatten() {
        // A link is not followed: that is how a search walks out of the
        // library, and how it goes round in a circle.
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if kind.is_dir() {
            if !crate::package::worth_walking(&name) {
                continue;
            }
            // A directory with a slave under it is one game, and is not
            // looked inside for more: a game's own data directories are
            // not each a game of their own.
            match crate::package::holds_a_slave(&path) {
                true => out.extend(relative(&path)),
                false => walk(root, &path, depth + 1, walked, out),
            }
            continue;
        }
        if crate::package::Kind::of_name(&name).is_some() {
            out.extend(relative(&path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let at = std::env::temp_dir().join(format!(
            "copperline-scan-{name}-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&at);
        std::fs::create_dir_all(&at).unwrap();
        at
    }

    /// Everything a scan says, run to a finish.
    fn drain(scan: &Scan) -> Vec<Progress> {
        let mut all = Vec::new();
        for _ in 0..2000 {
            all.extend(scan.poll());
            if all.last().is_some_and(Progress::is_last) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        all
    }

    #[test]
    fn a_scan_with_nothing_to_match_against_says_so_rather_than_failing_quietly() {
        // No session and no cached snapshot: the one case a scan cannot do
        // anything with. It has to name the reason, since "nothing
        // happened" is what a person would otherwise see.
        let games = scratch("nothing-games");
        std::fs::write(games.join("Zool_v1.0.lha"), b"x").unwrap();
        let cache = scratch("nothing-cache");

        let scan = Scan::start(games.clone(), cache.clone(), None, Default::default());
        let said = drain(&scan);
        let last = said.last().expect("a scan says something");
        assert!(
            matches!(last, Progress::Failed(why) if why.contains("Not logged in")),
            "{last:?}"
        );
        let _ = std::fs::remove_dir_all(&games);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_scan_matches_against_the_cache_without_a_session() {
        // Local first: once a catalogue is cached, a scan needs nothing
        // from the network -- which is also what makes it fast the second
        // time.
        use crate::gamelib::openretro::Record;
        let games = scratch("cached-games");
        for name in ["GoldenAxe_v1.5_0017.lha", "NoSuchGame_v1.0.lha"] {
            std::fs::write(games.join(name), b"x").unwrap();
        }
        let cache = scratch("cached-cache");
        let mut catalogue = Catalogue::new();
        catalogue.apply(&[Record {
            sync_id: 1,
            uuid: [1; 16],
            json: Some(r#"{"game_name":"Golden Axe","year":"1990"}"#.into()),
        }]);
        catalogue.save(&catalogue_path(&cache)).unwrap();

        let scan = Scan::start(games.clone(), cache.clone(), None, Default::default());
        let said = drain(&scan);
        assert!(
            matches!(said.last(), Some(Progress::Done { complete: true, .. })),
            "did not finish: {:?}",
            said.last()
        );
        // The result comes in Matched, before the art, so a scan stopped
        // after that point has still delivered everything but the
        // pictures.
        let Some(Progress::Matched { known, matched }) =
            said.iter().find(|p| matches!(p, Progress::Matched { .. }))
        else {
            panic!("nothing was matched: {said:?}");
        };
        assert_eq!(*matched, 1);
        assert_eq!(known.len(), 2);
        // Sorted comes later, in the store; here it is whatever the folder
        // gave, so find rather than index.
        let axe = known.iter().find(|k| k.file.starts_with("Golden")).unwrap();
        assert_eq!(
            axe.game.as_ref().map(|g| g.name.as_str()),
            Some("Golden Axe")
        );
        let other = known.iter().find(|k| k.file.starts_with("NoSuch")).unwrap();
        assert!(other.game.is_none(), "matched something it should not have");
        let _ = std::fs::remove_dir_all(&games);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_scan_of_a_folder_that_is_not_there_is_a_message_and_not_a_panic() {
        let cache = scratch("missing-cache");
        let scan = Scan::start(
            PathBuf::from("/no/such/folder"),
            cache.clone(),
            None,
            Default::default(),
        );
        let last = drain(&scan).pop().expect("a scan says something");
        assert!(matches!(last, Progress::Failed(_)), "{last:?}");
        // And an empty one is not confused with a missing one.
        let empty = scratch("missing-empty");
        let scan = Scan::start(empty.clone(), cache.clone(), None, Default::default());
        let last = drain(&scan).pop().expect("a scan says something");
        assert!(
            matches!(&last, Progress::Failed(why) if why.contains("No packages")),
            "{last:?}"
        );
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_stopped_scan_keeps_what_it_had() {
        // Pressing Run stops the scan. What it resolved up to then is worth
        // keeping: a library half filled in beats one not filled in at all,
        // and the next scan carries on from a cache that is further along.
        use crate::gamelib::openretro::Record;
        let games = scratch("stop-games");
        for i in 0..64 {
            std::fs::write(games.join(format!("Game{i}_v1.0.lha")), b"x").unwrap();
        }
        let cache = scratch("stop-cache");
        let mut catalogue = Catalogue::new();
        catalogue.apply(&[Record {
            sync_id: 1,
            uuid: [1; 16],
            json: Some(r#"{"game_name":"Game1"}"#.into()),
        }]);
        catalogue.save(&catalogue_path(&cache)).unwrap();

        let scan = Scan::start(games.clone(), cache.clone(), None, Default::default());
        scan.stop();
        let last = drain(&scan).pop().expect("a scan says something");
        // Either it stopped in time or it beat us to the end; both are
        // finished states, and neither loses anything.
        assert!(matches!(last, Progress::Done { .. }), "{last:?}");
        let _ = std::fs::remove_dir_all(&games);
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn a_scan_writes_its_art_where_the_launcher_reads_it() {
        // These drifted apart once -- the scan wrote cache/covers and the
        // page read cache -- and the symptom was a whole library of
        // downloaded art that never appeared. Both go through one
        // function now, and this is what says so.
        let cache = scratch("art-paths");
        let covers = covers_path(&cache);
        std::fs::create_dir_all(&covers).unwrap();
        let sha1 = "a".repeat(40);
        write_cover(&covers, &sha1, b"not really a png");

        let at = crate::gamelib::cover::cover_file(&covers, &sha1);
        assert!(at.is_file(), "the scan wrote nothing readable");
        assert!(at.starts_with(&covers), "the art left the covers directory");
        // And what a scan checks before fetching is the same file it wrote.
        assert!(!covers.join(format!("{sha1}.png.partial")).exists());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn the_search_finds_packages_all_the_way_down_but_not_through_a_link() {
        let dir = scratch("walk");
        std::fs::create_dir_all(dir.join("A/B")).unwrap();
        std::fs::write(dir.join("One_v1.0.lha"), b"x").unwrap();
        std::fs::write(dir.join("A/Two_v1.0.lha"), b"x").unwrap();
        std::fs::write(dir.join("A/B/Three_v1.0.LHA"), b"x").unwrap();
        std::fs::write(dir.join("A/notes.txt"), b"x").unwrap();
        #[cfg(unix)]
        {
            let outside = scratch("walk-outside");
            std::fs::write(outside.join("Away_v1.0.lha"), b"x").unwrap();
            std::os::unix::fs::symlink(&outside, dir.join("away")).unwrap();
        }
        let mut found = packages(&dir);
        found.sort();
        // Relative to the library, so two packages of the same name filed
        // under different letters stay two packages -- and the link is not
        // followed, so what is outside stays outside.
        assert_eq!(
            found,
            ["A/B/Three_v1.0.LHA", "A/Two_v1.0.lha", "One_v1.0.lha"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole thing against the live service and a real collection:
    /// sign in, sync, match, fetch the art, write the store, and read it
    /// back. Ignored; needs an account and somewhere to look.
    ///
    /// ```sh
    /// OPENRETRO_USER=you OPENRETRO_PASS=... WHDLOAD_GAMES=~/Games \
    ///   cargo test --release --lib scan_live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs an OpenRetro account and a directory of packages"]
    fn scan_live_end_to_end() {
        use crate::gamelib::{Database, Secret};

        let user = std::env::var("OPENRETRO_USER").expect("OPENRETRO_USER");
        let mut pass = Secret::new();
        for c in std::env::var("OPENRETRO_PASS")
            .expect("OPENRETRO_PASS")
            .chars()
        {
            pass.push(c);
        }
        let session =
            Session::open(&user, &pass, super::super::openretro::DEVICE_ID).expect("authorized");
        drop(pass);

        let games = PathBuf::from(std::env::var("WHDLOAD_GAMES").expect("WHDLOAD_GAMES"));
        let cache = std::env::var("COPPERLINE_SCAN_CACHE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| scratch("live-cache"));
        let started = std::time::Instant::now();
        let scan = Scan::start(
            games.clone(),
            cache.clone(),
            Some(Arc::new(session)),
            Default::default(),
        );
        let mut result = None;
        let mut complete = None;
        loop {
            for said in scan.poll() {
                eprintln!("  {}", said.message());
                match said {
                    Progress::Matched { known, matched } => result = Some((known, matched)),
                    Progress::Done { complete: done, .. } => complete = Some(done),
                    Progress::Failed(why) => panic!("the scan gave up: {why}"),
                    _ => {}
                }
            }
            if complete.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(complete, Some(true), "the scan stopped early");
        let (known, matched) = result.expect("the scan matched nothing");
        eprintln!(
            "scanned {} packages in {:.1}s, {matched} matched",
            known.len(),
            started.elapsed().as_secs_f32()
        );
        assert!(!known.is_empty(), "no packages in {}", games.display());

        // What it costs to keep and to open, which is the whole point of
        // the store's shape. Padded out to the worst case a real
        // collection reaches, since a few dozen would prove nothing.
        let mut db = Database::new();
        let mut padded = known.clone();
        for i in known.len()..5000 {
            let mut filler = known[i % known.len()].clone();
            filler.file = format!("filler/{i:05}_{}", filler.file);
            padded.push(filler);
        }
        db.set_known(padded);
        let at = cache.join("db.json");
        db.save(&at).expect("saved");
        let bytes = std::fs::metadata(&at).unwrap().len();
        let read = std::time::Instant::now();
        let back = Database::load(&at);
        let took = read.elapsed();
        eprintln!(
            "store: {} entries, {} KiB, opens in {:.1}ms",
            back.len(),
            bytes / 1024,
            took.as_secs_f32() * 1000.0
        );
        assert_eq!(back.len(), 5000);
        assert!(
            took < std::time::Duration::from_millis(250),
            "opening the page would stall: {took:?}"
        );
        // And every game still finds its own metadata after the round trip.
        for k in known.iter().filter(|k| k.game.is_some()) {
            assert_eq!(
                back.match_file(&k.file).map(|g| &g.name),
                k.game.as_ref().map(|g| &g.name),
                "{} lost its metadata",
                k.file
            );
        }

        // A second scan has nothing to fetch: the catalogue and the art are
        // both cached, which is what makes it the fast one.
        let again = std::time::Instant::now();
        let scan = Scan::start(games.clone(), cache.clone(), None, Default::default());
        let last = drain(&scan).pop().expect("a second scan finishes");
        assert!(
            matches!(last, Progress::Done { complete: true, .. }),
            "{last:?}"
        );
        eprintln!(
            "second scan, no network: {:.1}s",
            again.elapsed().as_secs_f32()
        );

        if std::env::var("COPPERLINE_SCAN_CACHE").is_err() {
            let _ = std::fs::remove_dir_all(&cache);
        }
    }

    /// What the sync actually carries, so matching is not designed from a
    /// guess about it.
    #[test]
    #[ignore = "diagnostic"]
    fn what_the_sync_carries() {
        use crate::gamelib::Secret;
        let user = std::env::var("OPENRETRO_USER").unwrap();
        let mut pass = Secret::new();
        for c in std::env::var("OPENRETRO_PASS").unwrap().chars() {
            pass.push(c);
        }
        let session =
            Session::open(&user, &pass, super::super::openretro::DEVICE_ID).expect("authorized");
        drop(pass);
        let mut keys: std::collections::BTreeMap<String, usize> = Default::default();
        let mut with_name = 0;
        let mut without = Vec::new();
        let mut whd: Vec<String> = Vec::new();
        let mut cursor = 0;
        for _ in 0..4 {
            let page = session.games_since(cursor).unwrap();
            if page.is_empty() {
                break;
            }
            cursor = page.last().unwrap().sync_id;
            for r in &page {
                let Some(json) = &r.json else { continue };
                let v: serde_json::Value = serde_json::from_str(json).unwrap();
                let Some(obj) = v.as_object() else { continue };
                for k in obj.keys() {
                    *keys.entry(k.clone()).or_default() += 1;
                }
                if obj.contains_key("whdload_url") && whd.len() < 4 {
                    whd.push(json.clone());
                }
                if obj.contains_key("game_name") {
                    with_name += 1;
                } else if without.len() < 3 {
                    without.push(json.clone());
                }
            }
        }
        session.close();
        eprintln!("fields across 4 pages ({with_name} with game_name):");
        for (k, n) in &keys {
            eprintln!("   {k}: {n}");
        }
        eprintln!("-- whdload records:");
        for j in &whd {
            eprintln!("   {}", &j[..j.len().min(700)]);
        }
        eprintln!("-- records without a game name:");
        for j in &without {
            eprintln!("   {}", &j[..j.len().min(600)]);
        }
    }

    /// How much of the catalogue could be matched by content rather than
    /// by name: the number that decides whether digests can lead.
    #[test]
    #[ignore = "diagnostic"]
    fn whdload_digest_coverage() {
        use crate::gamelib::Secret;
        let user = std::env::var("OPENRETRO_USER").unwrap();
        let mut pass = Secret::new();
        for c in std::env::var("OPENRETRO_PASS").unwrap().chars() {
            pass.push(c);
        }
        let session =
            Session::open(&user, &pass, super::super::openretro::DEVICE_ID).expect("authorized");
        drop(pass);
        let (mut games, mut variants, mut whd, mut with_dh0) = (0, 0, 0, 0);
        let mut slaves: std::collections::BTreeMap<String, String> = Default::default();
        let mut parents: std::collections::BTreeSet<String> = Default::default();
        let mut cursor = 0;
        loop {
            let page = session.games_since(cursor).unwrap();
            if page.is_empty() {
                break;
            }
            cursor = page.last().unwrap().sync_id;
            for r in &page {
                let Some(json) = &r.json else { continue };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
                    continue;
                };
                let Some(obj) = v.as_object() else { continue };
                if obj.contains_key("game_name") {
                    games += 1;
                    continue;
                }
                variants += 1;
                if obj.contains_key("dh0_sha1") {
                    with_dh0 += 1;
                }
                let Some(list) = obj.get("file_list").and_then(|f| f.as_str()) else {
                    continue;
                };
                let Ok(files) = serde_json::from_str::<serde_json::Value>(list) else {
                    continue;
                };
                let mut had_slave = false;
                for f in files.as_array().into_iter().flatten() {
                    let (Some(name), Some(sha1)) = (
                        f.get("name").and_then(|n| n.as_str()),
                        f.get("sha1").and_then(|s| s.as_str()),
                    ) else {
                        continue;
                    };
                    if name.to_lowercase().ends_with(".slave") {
                        had_slave = true;
                        slaves.insert(sha1.to_string(), name.to_string());
                    }
                }
                if had_slave {
                    whd += 1;
                    if let Some(p) = obj.get("parent_uuid").and_then(|p| p.as_str()) {
                        parents.insert(p.to_string());
                    }
                }
            }
        }
        session.close();
        eprintln!(
            "catalogue: {games} games, {variants} variants, {whd} WHDLoad variants \
             ({with_dh0} with dh0_sha1), {} distinct slave digests covering {} games",
            slaves.len(),
            parents.len()
        );
    }

    /// The match rate and what the loose passes cost, against a real
    /// library and the cached catalogue.
    #[test]
    #[ignore = "needs a scanned cache and a directory of packages"]
    fn match_rate_on_a_real_library() {
        let cache = PathBuf::from(std::env::var("COPPERLINE_SCAN_CACHE").unwrap());
        let catalogue = Catalogue::load(&catalogue_path(&cache));
        let games = PathBuf::from(std::env::var("WHDLOAD_GAMES").unwrap());
        let files = packages(&games);
        let started = std::time::Instant::now();
        let mut matched = 0;
        let mut misses = Vec::new();
        let mut loose = Vec::new();
        for file in &files {
            let base = file.rsplit('/').next().unwrap_or(file);
            match catalogue.match_file(base) {
                Some(game) => {
                    matched += 1;
                    // Only the ones no key matched exactly: the loose
                    // passes are the ones worth eyeballing.
                    if catalogue.matched_exactly(base).is_none() {
                        loose.push(format!("{base}  ->  {}", game.name));
                    }
                }
                None => misses.push(base.to_string()),
            }
        }
        let took = started.elapsed();
        eprintln!(
            "{matched}/{} matched ({}%) in {:.0}ms over {} catalogue games",
            files.len(),
            matched * 100 / files.len(),
            took.as_secs_f32() * 1000.0,
            catalogue.len()
        );
        eprintln!("-- {} matched loosely:", loose.len());
        for l in loose.iter().take(30) {
            eprintln!("   {l}");
        }
        for m in misses.iter().take(4) {
            eprintln!("   miss {m}");
        }
    }

    /// Read back the art a real scan left, through the launcher's own path.
    #[test]
    #[ignore = "needs a scanned cache"]
    fn art_on_disk_is_found_by_the_reader() {
        let cache = PathBuf::from(std::env::var("COPPERLINE_SCAN_CACHE").unwrap());
        let db = crate::gamelib::Database::load(&PathBuf::from(
            std::env::var("COPPERLINE_LIBRARY_DB").unwrap(),
        ));
        let mut covers = crate::gamelib::Covers::new(covers_path(&cache));
        // Only the ones whose file is actually there: a store that has
        // moved ahead of its cache -- better matching naming art nothing
        // has fetched yet -- is a scan away from being right, not a bug.
        let wanted: Vec<String> = db
            .known()
            .iter()
            .filter_map(|k| k.game.as_ref()?.front_sha1.clone())
            .filter(|sha1| crate::gamelib::cover::cover_file(&covers_path(&cache), sha1).is_file())
            // As many as the queue holds: it drops stale requests on
            // purpose, so asking for a hundred at once would prove
            // nothing about the ones it kept.
            .take(6)
            .collect();
        assert!(!wanted.is_empty(), "no art on disk to read back");
        for sha1 in &wanted {
            covers.want(sha1);
        }
        let mut have = 0;
        for _ in 0..200 {
            covers.poll();
            have = wanted.iter().filter(|s| covers.get(s).is_some()).count();
            if have == wanted.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        eprintln!(
            "{have}/{} covers read back from {}",
            wanted.len(),
            cache.display()
        );
        assert_eq!(have, wanted.len(), "art on disk was not found");
    }

    /// Refresh against a real folder: a duplicate lists twice, and
    /// deleting one takes it out.
    #[test]
    fn refresh_follows_the_folder() {
        use crate::gamelib::Database;
        let dir = scratch("refresh");
        for name in ["GoldenAxe_v1.lha", "GoldenAxe_v1.zip", "Zool_v1.lha"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let mut db = Database::new();
        db.merge_found(packages(&dir));
        let mut found: Vec<&str> = db.known().iter().map(|k| k.file.as_str()).collect();
        found.sort();
        assert_eq!(
            found,
            ["GoldenAxe_v1.lha", "GoldenAxe_v1.zip", "Zool_v1.lha"]
        );

        std::fs::remove_file(dir.join("GoldenAxe_v1.zip")).unwrap();
        db.merge_found(packages(&dir));
        let mut found: Vec<&str> = db.known().iter().map(|k| k.file.as_str()).collect();
        found.sort();
        assert_eq!(found, ["GoldenAxe_v1.lha", "Zool_v1.lha"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
