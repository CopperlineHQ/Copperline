// SPDX-License-Identifier: GPL-3.0-or-later

//! The two stores behind the Library page, and the name matching that ties
//! a package on disk to a catalogued game.
//!
//! **[`Database`]** -- `library_db`, one entry per package that a scan
//! found, with the metadata it resolved. This is what opening the page
//! reads, so it is built to be read: entries sorted by file name, looked up
//! by binary search, and no index rebuilt after parsing. A library of a few
//! thousand games is a few hundred kilobytes and parses in a few
//! milliseconds. One entry to a line, so a game that matched wrongly can be
//! found with a text editor -- the parser does not care where the newlines
//! are, so that costs nothing to read.
//!
//! **[`Catalogue`]** -- the snapshot of the online database a scan matched
//! against, kept in `library_cache` because it is downloaded and can be
//! thrown away. Several megabytes, and touched only while a scan runs, so
//! opening the page never reads it. The sync cursor lives in it, which is
//! what lets the next scan ask for the changes rather than all of it again.
//!
//! Both are written through a temporary and a rename, so a write
//! interrupted half way leaves the previous file rather than half of a new
//! one.
//!
//! Only *game* records are kept. The sync also carries a variant per
//! release -- disk images with their SHA1s -- which is how a launcher that
//! browses ADFs identifies them by content. A WHDLoad package shares no
//! bytes with those images, so none of that would help here and none of it
//! is stored; matching is by name, and [`match_key`] is the whole of it.

use std::collections::HashMap;
use std::path::Path;

/// The scanned library's format. A file written by a different one is
/// discarded and rescanned rather than read: a wrong guess about an old
/// layout is a library full of quietly wrong metadata.
const FORMAT: u32 = 1;

/// The catalogue snapshot's format, which moves independently: it is a
/// cache, and throwing it away costs only a download.
const CATALOGUE_FORMAT: u32 = 1;

/// One game, as the Library page needs it.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Game {
    /// What the entry is, stably: the key a later sync updates in place.
    /// Empty in a scanned library, where the file name is the key and the
    /// uuid would be bytes on disk nothing reads.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uuid: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub players: Option<String>,
    /// Which release of the game this package is.
    ///
    /// The one field the catalogue has no opinion about: there is no
    /// standard to how installers name a release, so
    /// `CannonFodder2_v1.12_Fr_2578` and `CannonFodder2_v1.11_0104` are
    /// the same catalogued game and nothing in the record separates them.
    /// A person can put whatever tells them apart in here -- "CD32 v1.1"
    /// -- and where a library holds several under one title the page
    /// offers the file name, which is the only honest answer to hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The digest of the cover art, to fetch it by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub front_sha1: Option<String>,
}

/// One package a scan found, and what it turned out to be.
///
/// Flat rather than a flattened `Game`: `#[serde(flatten)]` buffers each
/// record through an intermediate map, which is exactly the cost this store
/// is shaped to avoid.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Known {
    /// The package's file name, with its extension: what the scan saw, and
    /// the key this store is sorted and searched by.
    pub file: String,
    /// What the catalogue said about it. Absent where the scan found no
    /// match -- kept anyway, so a rescan does not look the same name up
    /// again and a person can see that it was tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<Game>,
    /// The digest of the package's WHDLoad slave, once it has been read.
    ///
    /// Kept so a second scan does not open every archive again: reading it
    /// means decompressing one member out of each package, which is the
    /// slowest thing a scan does that is not the network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slave_sha1: Option<String>,
    /// Set where a person filled this in themselves. A scan leaves those
    /// alone: somebody who has corrected a wrong match, or given a game
    /// its own cover, does not want the next scan putting it back. Clearing
    /// the entry and saving it empty puts it back in the scan's hands.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub manual: bool,
}

/// The scanned library: what a scan found, and which of it is a favourite.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Database {
    format: u32,
    /// The packages marked as favourites: the path the store files each
    /// under, against the name to show for it.
    ///
    /// Kept here rather than in the machine configuration because a
    /// favourite is not a setting of any machine. Keyed by the package
    /// rather than by the game, because a collection holds the same game
    /// several times over and starring one of them should star that one --
    /// marking every release of Cannon Fodder 2 because one was picked is
    /// not what anybody meant. The name is kept with it so a favourite
    /// whose package has been deleted still reads as a game rather than as
    /// a path, which is the state it most needs to be removable from.
    #[serde(default)]
    favourites: std::collections::BTreeMap<String, String>,
    /// Sorted by `file`, which is what makes this store fast to open: the
    /// order comes off disk with the parse and lookup is a binary search,
    /// so there is no index to build before the page can draw.
    #[serde(default)]
    known: Vec<Known>,
}

/// An empty library carries this format, not zero: it is about to be
/// written, and a store that says it came from format 0 would be discarded
/// the next time it was read.
impl Default for Database {
    fn default() -> Database {
        Database {
            format: FORMAT,
            favourites: std::collections::BTreeMap::new(),
            known: Vec::new(),
        }
    }
}

impl Database {
    /// An empty library, as though nothing had ever been scanned.
    pub fn new() -> Database {
        Database::default()
    }

    /// Read the store, or start empty.
    ///
    /// A file that cannot be read, cannot be parsed, or was written by
    /// another format is not an error to report: it means there is nothing
    /// usable held, which is the same position as never having scanned. The
    /// page lists the folder without metadata, and a scan writes a good one.
    pub fn load(path: &Path) -> Database {
        let mut db = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Database>(&text).ok())
            .filter(|db| db.format == FORMAT)
            .unwrap_or_default();
        // Trusted from the file where it is already right, which is the
        // usual case, and put right where it is not: a hand-edited store
        // must not silently stop matching.
        if !db.known.windows(2).all(|w| w[0].file <= w[1].file) {
            db.known.sort_by(|a, b| a.file.cmp(&b.file));
        }
        db
    }

    /// Write the store, through a temporary file so an interrupted write
    /// leaves the previous library rather than half of a new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(path, &to_readable_json(self)?)
    }

    /// Replace what is known with the result of a scan.
    ///
    /// Entries a person filled in themselves survive it, which is the
    /// whole point of marking one manual: a scan must not undo a
    /// correction somebody made because the catalogue had the wrong game.
    ///
    /// They survive a rename, too. A package matched by file name alone
    /// would lose its corrections the moment somebody tidied their
    /// collection; matched by the digest of its slave, the same game is
    /// the same game whatever it has been called since. The digest is what
    /// the scan worked out on the way past -- see
    /// [`crate::gamelib::scan`] -- so this costs a lookup and no reading.
    pub fn set_known(&mut self, mut known: Vec<Known>) {
        known.sort_by(|a, b| a.file.cmp(&b.file));
        // What was hand-filled, by digest, for the ones whose names have
        // changed. Built only when there is something to carry across.
        let renamed: HashMap<&str, &Known> = self
            .known
            .iter()
            .filter(|held| held.manual)
            .filter_map(|held| Some((held.slave_sha1.as_deref()?, held)))
            .collect();
        for entry in &mut known {
            let held = self
                .known
                .binary_search_by(|k| k.file.as_str().cmp(entry.file.as_str()))
                .ok()
                .map(|at| &self.known[at])
                .filter(|held| held.manual)
                .or_else(|| {
                    entry
                        .slave_sha1
                        .as_deref()
                        .and_then(|sha1| renamed.get(sha1).copied())
                });
            let Some(held) = held else { continue };
            // The file name is this scan's -- the package really is called
            // that now -- and everything a person chose comes across.
            *entry = Known {
                file: entry.file.clone(),
                slave_sha1: entry.slave_sha1.clone().or_else(|| held.slave_sha1.clone()),
                game: held.game.clone(),
                manual: true,
            };
        }
        self.known = known;
    }

    /// What the store holds for one package, metadata and manual flag both.
    pub fn entry(&self, file: &str) -> Option<&Known> {
        self.known
            .binary_search_by(|k| k.file.as_str().cmp(file))
            .ok()
            .map(|at| &self.known[at])
    }

    /// Put one entry in, replacing whatever was there.
    pub fn set_entry(&mut self, entry: Known) {
        match self.known.binary_search_by(|k| k.file.cmp(&entry.file)) {
            Ok(at) => self.known[at] = entry,
            Err(at) => self.known.insert(at, entry),
        }
    }

    /// Bring the store into line with what is on disk: `files` is every
    /// package the folder now holds.
    ///
    /// What was already resolved is carried across, so re-reading the
    /// folder is not a way to lose the metadata a scan spent minutes
    /// fetching. New packages arrive unresolved, and packages that have
    /// gone are dropped. Answers how many are new, which is also how much
    /// work a scan after this has to do.
    pub fn merge_found(&mut self, files: Vec<String>) -> usize {
        let mut fresh = 0;
        let mut known: Vec<Known> = files
            .into_iter()
            .map(|file| {
                let held = self
                    .known
                    .binary_search_by(|k| k.file.as_str().cmp(file.as_str()))
                    .ok()
                    .map(|at| &self.known[at]);
                let game = held.and_then(|held| held.game.clone());
                let manual = held.is_some_and(|held| held.manual);
                // The digest survives too: it is the expensive thing to
                // work out, and re-reading a folder has not changed it.
                let slave_sha1 = held.and_then(|held| held.slave_sha1.clone());
                fresh += usize::from(game.is_none());
                Known {
                    file,
                    game,
                    slave_sha1,
                    manual,
                }
            })
            .collect();
        known.sort_by(|a, b| a.file.cmp(&b.file));
        self.known = known;
        fresh
    }

    /// How many packages the last scan found.
    pub fn len(&self) -> usize {
        self.known.len()
    }

    pub fn is_empty(&self) -> bool {
        self.known.is_empty()
    }

    /// Everything the last scan found.
    pub fn known(&self) -> &[Known] {
        &self.known
    }

    /// Whether a package is a favourite.
    pub fn is_favourite(&self, file: &str) -> bool {
        self.favourites.contains_key(file)
    }

    /// Mark or unmark a favourite, answering whether it is one now.
    /// `title` is what to call it in the favourites list, which matters
    /// once the game itself is gone and there is nothing left to ask.
    pub fn toggle_favourite(&mut self, file: &str, title: &str) -> bool {
        if self.favourites.remove(file).is_some() {
            false
        } else {
            self.favourites.insert(file.to_string(), title.to_string());
            true
        }
    }

    /// Drop a favourite by its key, whether or not its game is still
    /// there. This is how one is taken off after its package has been
    /// deleted, when there is nothing in the library left to untick.
    pub fn remove_favourite(&mut self, key: &str) {
        self.favourites.remove(key);
    }

    /// The favourites, as (key, name), in the order they are listed.
    pub fn favourites(&self) -> impl Iterator<Item = (&str, &str)> {
        self.favourites
            .iter()
            .map(|(key, name)| (key.as_str(), name.as_str()))
    }

    /// A file name to keep a package's own cover art under.
    ///
    /// Per package rather than per game: two releases of one game are two
    /// entries and may want two pictures. Reduced to letters and digits
    /// so it is a name every filesystem accepts, and prefixed so hand-set
    /// art is never mistaken for a catalogue digest.
    pub fn art_key(file: &str) -> String {
        // A digest rather than the path with its punctuation flattened:
        // mapping every separator to `_` makes `A/B.lha` and `A_B.lha` the
        // same file name, and the two would overwrite each other's cover.
        format!("manual-{}", crate::gamelib::sha1::hex(file.as_bytes()))
    }

    /// How many are marked, so a page can tell an empty list from a
    /// database that has never been synced.
    pub fn favourite_count(&self) -> usize {
        self.favourites.len()
    }

    /// What the last scan resolved for a package, by its exact file name.
    ///
    /// Exact rather than by match key: the scan already did the matching,
    /// and this is reading back its answer for the file it answered about.
    pub fn match_file(&self, file_name: &str) -> Option<&Game> {
        self.known
            .binary_search_by(|k| k.file.as_str().cmp(file_name))
            .ok()
            .and_then(|at| self.known[at].game.as_ref())
    }
}

/// The snapshot of the online database a scan matches against.
///
/// Lives in the cache directory: it is downloaded, it is large, and losing
/// it costs a download and nothing else.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Catalogue {
    format: u32,
    /// The digest of a WHDLoad slave against the game it belongs to.
    ///
    /// The sync carries a record per release as well as one per game, and
    /// a WHDLoad release lists its installed files with a SHA-1 each. The
    /// slave is the one worth keeping: it is small, it is always there,
    /// and it names the game exactly. Matching a package by it is an
    /// answer rather than a guess -- see [`Catalogue::match_digest`].
    #[serde(default)]
    digests: std::collections::BTreeMap<String, String>,
    /// The highest sync id taken, which the next sync asks to continue
    /// after. Zero means nothing has been taken yet.
    pub cursor: u32,
    pub games: Vec<Game>,
    /// Built from `games` on load; never stored, because they are derived
    /// and a stored index is one more thing that can disagree with the
    /// data.
    #[serde(skip)]
    by_key: HashMap<String, usize>,
    #[serde(skip)]
    by_uuid: HashMap<String, usize>,
    /// Each game's title reduced once, for the two passes that have to
    /// look at every entry rather than hash straight to one.
    #[serde(skip)]
    reduced: Vec<Reduced>,
}

/// A catalogue title as the matching sees it.
#[derive(Debug, Clone, Default)]
struct Reduced {
    /// The words, normalised, in order.
    words: Vec<String>,
    /// Those words run together: the key, and what the edit distance is
    /// measured across.
    key: String,
}

impl Default for Catalogue {
    fn default() -> Catalogue {
        Catalogue {
            format: CATALOGUE_FORMAT,
            cursor: 0,
            digests: std::collections::BTreeMap::new(),
            games: Vec::new(),
            by_key: HashMap::new(),
            by_uuid: HashMap::new(),
            reduced: Vec::new(),
        }
    }
}

impl Catalogue {
    pub fn new() -> Catalogue {
        Catalogue::default()
    }

    /// Read the snapshot, or start empty and re-sync from the beginning.
    pub fn load(path: &Path) -> Catalogue {
        let mut cat = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Catalogue>(&text).ok())
            .filter(|cat| cat.format == CATALOGUE_FORMAT)
            .unwrap_or_default();
        cat.reindex();
        cat
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_json(path, &catalogue_json(self)?)
    }

    pub fn len(&self) -> usize {
        self.games.len()
    }

    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// The game a slave's digest belongs to.
    ///
    /// Exact where it answers: the bytes of that slave are that game's, and
    /// nothing about the file's name comes into it. Tried before the name
    /// matching, which is the guess this replaces where it can.
    pub fn match_digest(&self, sha1: &str) -> Option<&Game> {
        let uuid = self.digests.get(sha1)?;
        self.by_uuid.get(uuid).map(|&at| &self.games[at])
    }

    /// How many slave digests are held, for reporting what a scan has to
    /// work with.
    pub fn digest_count(&self) -> usize {
        self.digests.len()
    }

    /// The game a package's file name names, if the catalogue knows it.
    pub fn match_file(&self, file_name: &str) -> Option<&Game> {
        let title = strip_package_name(file_name);
        let (plain, folded) = match_keys(&title);
        let exact = self
            .by_key
            .get(&plain)
            .or_else(|| folded.and_then(|folded| self.by_key.get(&folded)));
        match exact {
            Some(&at) => Some(&self.games[at]),
            // Only what nothing matched exactly walks the catalogue, which
            // on a real library is a tenth of it.
            None => self.match_loosely(&title),
        }
    }

    /// Fold one page of sync records in, answering how many entries it
    /// changed. Zero from the first page of a sync is what "already up to
    /// date" is read from.
    ///
    /// Records without a game name are the per-release variants, which
    /// carry disk-image digests a WHDLoad package cannot match: they are
    /// skipped rather than stored.
    pub fn apply(&mut self, records: &[super::openretro::Record]) -> usize {
        let mut changed = 0;
        for record in records {
            self.cursor = self.cursor.max(record.sync_id);
            let uuid = hex(&record.uuid);
            let Some(json) = &record.json else {
                // A deletion: the entry has gone from the database.
                if let Some(at) = self.games.iter().position(|g| g.uuid == uuid) {
                    self.games.remove(at);
                    changed += 1;
                }
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
                continue;
            };
            let Some(name) = value.get("game_name").and_then(|v| v.as_str()) else {
                // A release rather than a game. Most carry disk images,
                // which a WHDLoad package shares no bytes with; the
                // WHDLoad ones carry the installed files, and their slave
                // digests are what makes an exact match possible.
                self.take_slave_digests(&value);
                continue;
            };
            if name.trim().is_empty() {
                continue;
            }
            let field = |key: &str| -> Option<String> {
                value
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let game = Game {
                uuid: uuid.clone(),
                name: name.trim().to_string(),
                year: field("year"),
                publisher: field("publisher"),
                developer: field("developer"),
                players: field("players"),
                // Not a catalogue field: the sync has no opinion about
                // which release a package is.
                version: None,
                front_sha1: field("front_sha1"),
            };
            match self.games.iter_mut().find(|g| g.uuid == uuid) {
                Some(held) if *held == game => {}
                Some(held) => {
                    *held = game;
                    changed += 1;
                }
                None => {
                    self.games.push(game);
                    changed += 1;
                }
            }
        }
        // Only the games are indexed. A page of nothing but release
        // records has added digests, and those go into a map of their own
        // -- there is nothing for a rebuild to do, and a rebuild is every
        // title in the catalogue taken apart again.
        if changed > 0 {
            self.reindex();
        }
        changed
    }

    /// Rebuild the name index. The first entry to claim a key keeps it, so
    /// a later re-import of the same game cannot displace what is matching
    /// already.
    /// Note the slave digests a release record carries, against the game
    /// it is a release of.
    fn take_slave_digests(&mut self, value: &serde_json::Value) {
        let Some(parent) = value.get("parent_uuid").and_then(|v| v.as_str()) else {
            return;
        };
        // `file_list` is a JSON array inside a JSON string.
        let Some(list) = value.get("file_list").and_then(|v| v.as_str()) else {
            return;
        };
        let Ok(files) = serde_json::from_str::<serde_json::Value>(list) else {
            return;
        };
        for file in files.as_array().into_iter().flatten() {
            let (Some(name), Some(sha1)) = (
                file.get("name").and_then(|n| n.as_str()),
                file.get("sha1").and_then(|s| s.as_str()),
            ) else {
                continue;
            };
            if name.to_ascii_lowercase().ends_with(".slave") && sha1.len() == 40 {
                self.digests
                    .insert(sha1.to_ascii_lowercase(), parent.to_string());
            }
        }
    }

    fn reindex(&mut self) {
        self.games.sort_by(|a, b| a.name.cmp(&b.name));
        self.by_uuid.clear();
        self.by_uuid.extend(
            self.games
                .iter()
                .enumerate()
                .map(|(at, game)| (game.uuid.clone(), at)),
        );
        self.by_key.clear();
        self.reduced.clear();
        self.reduced.reserve(self.games.len());
        for (at, game) in self.games.iter().enumerate() {
            let (plain, folded) = match_keys(&game.name);
            self.reduced.push(Reduced {
                words: title_words_full(&game.name),
                key: plain.clone(),
            });
            self.by_key.entry(plain).or_insert(at);
            if let Some(folded) = folded {
                self.by_key.entry(folded).or_insert(at);
            }
        }
    }

    /// The game a package's name matches exactly, for telling an exact
    /// match from a loose one.
    pub fn matched_exactly(&self, file_name: &str) -> Option<&Game> {
        let (plain, folded) = match_keys(&strip_package_name(file_name));
        self.by_key
            .get(&plain)
            .or_else(|| folded.and_then(|folded| self.by_key.get(&folded)))
            .map(|&at| &self.games[at])
    }

    /// The two passes for a package no key matched exactly.
    ///
    /// Both are deliberately stricter than a scraper a person watches
    /// would be: a wrong match writes a wrong game into somebody's library
    /// and stays there, where a missing one is visibly missing.
    ///
    /// **Words in order.** A catalogue title every one of whose words
    /// appears, in that order, inside the package's name. This is what
    /// finds `IndianaJones&TheLastCrusadeAction` -- the package is the
    /// catalogue title plus a word saying which of the two games it is.
    /// The longest such title wins, so the more specific entry is
    /// preferred over one that happens to be a prefix of it. Order is what
    /// makes it safe: without it, "World Rugby" matches
    /// `RugbyTheWorldCup`, which is a different game.
    ///
    /// **Edit distance.** For the rest -- `RBI2Baseball` against "R.B.I.
    /// Two Baseball", `UniversalMilitarySimulator` against "UMS (Universal
    /// Military Simulator)". Skyscraper accepts 65% here; measured against
    /// a real library that is too loose (it pairs `UltimateGolf` with "The
    /// Ultimate Quiz" at 66), so this wants 80.
    fn match_loosely(&self, title: &str) -> Option<&Game> {
        let words = title_words_full(title);
        let key: String = title_words(title).concat();
        if key.len() < MIN_LOOSE_KEY {
            return None;
        }
        let ordered = self
            .reduced
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.words.len() >= MIN_LOOSE_WORDS
                    && r.words.iter().map(String::len).sum::<usize>() >= MIN_LOOSE_KEY
                    && is_subsequence(&r.words, &words)
            })
            .max_by_key(|(_, r)| r.words.iter().map(String::len).sum::<usize>());
        if let Some((at, _)) = ordered {
            return Some(&self.games[at]);
        }
        self.reduced
            .iter()
            .enumerate()
            // The length gate first: it is a comparison, and the distance
            // it saves is a matrix.
            .filter(|(_, r)| {
                r.key.len() >= MIN_LOOSE_KEY && r.key.len().abs_diff(key.len()) <= LOOSE_SLACK
            })
            .map(|(at, r)| (similarity(&key, &r.key), at))
            .filter(|&(score, _)| score >= MIN_SIMILARITY)
            .max_by_key(|&(score, _)| score)
            .map(|(_, at)| &self.games[at])
    }
}

/// A catalogue title needs at least this many words, and this many
/// characters, before either loose pass will consider it. Without the
/// guard a one-word entry is a subsequence of nearly everything: "War"
/// swallows `UMS2NationsAtWar`, and "1990" swallows
/// `Italy1990WinnersEdition`.
const MIN_LOOSE_WORDS: usize = 2;
const MIN_LOOSE_KEY: usize = 10;
/// How far apart two keys may be in length and still be worth measuring.
const LOOSE_SLACK: usize = 6;
/// How alike they must then be, as a percentage.
const MIN_SIMILARITY: usize = 80;

/// Whether `needle` appears in `haystack` in order, other words allowed
/// between and after.
fn is_subsequence(needle: &[String], haystack: &[String]) -> bool {
    let mut want = needle.iter();
    let mut next = want.next();
    for word in haystack {
        match next {
            Some(n) if n == word => next = want.next(),
            _ => {}
        }
    }
    next.is_none()
}

/// How alike two keys are, 0 to 100, by Levenshtein distance over the
/// longer of them.
fn similarity(a: &str, b: &str) -> usize {
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0;
    }
    100 * (longest - edit_distance(a.as_bytes(), b.as_bytes())) / longest
}

/// Levenshtein distance, two rows rather than a matrix: the keys are
/// alphanumeric ASCII by construction, so bytes are characters.
fn edit_distance(a: &[u8], b: &[u8]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0usize; b.len() + 1];
    for (i, &x) in a.iter().enumerate() {
        row[0] = i + 1;
        for (j, &y) in b.iter().enumerate() {
            row[j + 1] = (prev[j] + usize::from(x != y))
                .min(prev[j + 1] + 1)
                .min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

/// A game's name reduced to the one thing two spellings of it share.
///
/// A WHDLoad package is named by its installer and a database entry by a
/// cataloguer, so the same game arrives as `JamesPond2_v2.0_AGA_1354` and
/// as "James Pond II: Robocod". What survives both is the letters and
/// digits, in order, with the numbering written one way:
///
/// - case and every separator go, so `JamesPond2` and "James Pond 2" meet;
/// - a trailing roman numeral of two letters or more becomes its digits,
///   so "II" meets `2`;
/// - an ampersand becomes "and";
/// - a leading article goes, so "The Settlers" meets `Settlers`;
/// - anything after a colon goes, since a package name carries the title
///   and not the subtitle.
pub fn match_key(name: &str) -> String {
    keys(name).0
}

/// Every spelling of a name worth filing it under.
///
/// A trailing single letter is the awkward case, and it cannot be decided
/// from one side alone: "King's Quest V" is the fifth King's Quest, and
/// `RanX` is a game called Ranx. Rather than guess, a name that ends in
/// one gets both keys -- the letter as a letter, and the letter as a
/// number. The catalogue is filed under both and a package is looked up by
/// both, so whichever way round the two spellings fall, they meet.
///
/// The first is the primary: what a favourite is filed under, where one
/// answer is needed and the letter is the likelier reading.
pub fn match_keys(name: &str) -> (String, Option<String>) {
    keys(name)
}

fn keys(name: &str) -> (String, Option<String>) {
    let plain = build_key(name, false);
    let folded = build_key(name, true);
    (plain.clone(), (folded != plain).then_some(folded))
}

/// `fold_single` decides whether a trailing single-letter roman numeral
/// becomes its digits.
fn build_key(name: &str, fold_single: bool) -> String {
    let words = title_words(name);
    let words = words.as_slice();
    let mut out = String::new();
    for (i, word) in words.iter().enumerate() {
        let last = i + 1 == words.len();
        match roman_to_arabic(word, fold_single && last && i > 0) {
            Some(n) => out.push_str(&n.to_string()),
            None => out.push_str(word),
        }
    }
    out
}

/// A title reduced to its words: the one place a name is taken apart, so
/// the key and the two loose passes can never disagree about what the
/// words of a title are.
///
/// Everything after a colon goes -- a package name carries the title and
/// not the subtitle -- an ampersand becomes "and", humps become spaces,
/// punctuation goes, and a leading or trailing article goes with it.
fn title_words(name: &str) -> Vec<String> {
    title_parts(name, true)
}

/// The same, keeping the subtitle.
///
/// The exact key drops it, because a package name carries the title and
/// not the subtitle. The words-in-order pass wants it: an installer that
/// ran the two together -- `RobinHoodLegendQuest` for "Robin Hood: Legend
/// Quest" -- has the subtitle's words right there, and asking for them is
/// a stricter test rather than a looser one.
fn title_words_full(name: &str) -> Vec<String> {
    title_parts(name, false)
}

fn title_parts(name: &str, cut_subtitle: bool) -> Vec<String> {
    let title = match cut_subtitle {
        true => name.split(':').next().unwrap_or(name),
        false => name,
    };
    // An ampersand is a word: installers write `Utopia&NewWorlds` where a
    // cataloguer writes "Utopia and New Worlds". Dropping it as
    // punctuation makes those two disagree by three letters.
    let spaced = split_camel_case(&title.replace('&', " and "));
    let words: Vec<String> = spaced
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect();
    // A catalogue writes the article at the end -- "Settlers, The" -- and
    // an installer writes it at the front or not at all, so it is dropped
    // from either end. Not from a title that is only an article.
    let article = |w: &String| matches!(w.as_str(), "the" | "a" | "an");
    match words.as_slice() {
        [_] => words,
        [rest @ .., last] if article(last) => rest.to_vec(),
        [first, rest @ ..] if article(first) => rest.to_vec(),
        _ => words,
    }
}

/// Put a space at each hump, so `JamesPond2` becomes `James Pond 2` and
/// the words can be looked at one at a time. A run of capitals is one word
/// (`AGA`), and a digit starts a new one.
fn split_camel_case(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        let starts_word = match prev {
            None => false,
            Some(p) => {
                // aB, 1a, a1 -- and the last capital of a run before a
                // lowercase word, as in `AGAVersion`.
                (p.is_lowercase() && c.is_uppercase())
                    || (p.is_numeric() != c.is_numeric() && c.is_alphanumeric())
                    || (p.is_uppercase()
                        && c.is_uppercase()
                        && next.is_some_and(char::is_lowercase))
            }
        };
        if starts_word {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A roman numeral as its number, so that "II" and `2` are the same word.
/// Only the small ones a game title uses, and only when the whole word is
/// one: `mix` and `civ` are words, not numerals.
///
/// A single letter is the hard case. "I", "V" and "X" are words as often
/// as numbers, so one is read as a numeral only in the place a sequel
/// number goes: the last word of a title that has other words before it.
/// "King's Quest V" is the fifth; "V" and "I Game" are not.
fn roman_to_arabic(word: &str, in_numeral_position: bool) -> Option<u32> {
    const NUMERALS: [(&str, u32); 20] = [
        ("i", 1),
        ("ii", 2),
        ("iii", 3),
        ("iv", 4),
        ("v", 5),
        ("vi", 6),
        ("vii", 7),
        ("viii", 8),
        ("ix", 9),
        ("x", 10),
        ("xi", 11),
        ("xii", 12),
        ("xiii", 13),
        ("xiv", 14),
        ("xv", 15),
        ("xvi", 16),
        ("xvii", 17),
        ("xviii", 18),
        ("xix", 19),
        ("xx", 20),
    ];
    if word.chars().count() < 2 && !in_numeral_position {
        return None;
    }
    NUMERALS
        .iter()
        .find(|(numeral, _)| *numeral == word)
        .map(|&(_, n)| n)
}

/// A WHDLoad archive's file name reduced to the game's name.
///
/// Installers name a package for the game and then say which install it
/// is: `GoldenAxe_v1.5_0017.lha`, `JamesPond2_v2.0_AGA_1354.lha`. The
/// version is where the title stops, and everything after it describes the
/// install rather than the game.
pub fn strip_package_name(file_name: &str) -> String {
    // Only a package extension comes off. A game that arrives as a plain
    // folder has none, and plenty of Amiga titles have a dot in the name
    // itself -- `S.W.I.V`, `R.B.I. 2 Baseball` -- so cutting at the last
    // dot would take letters off the title.
    let stem = crate::package::stem_of(file_name);
    // Cut at the first `_v<digit>` -- the version the installer stamped on.
    let mut cut = stem.len();
    let bytes: Vec<char> = stem.chars().collect();
    for i in 0..bytes.len().saturating_sub(2) {
        if bytes[i] == '_'
            && (bytes[i + 1] == 'v' || bytes[i + 1] == 'V')
            && bytes[i + 2].is_ascii_digit()
        {
            cut = i;
            break;
        }
    }
    let title: String = bytes[..cut.min(bytes.len())].iter().collect();
    title.replace('_', " ")
}

/// A store as text: the header on its own lines, then one record to a
/// line, so a person can find a game with a text editor and a search.
///
/// Not `to_string_pretty`, which would put every field of every record on
/// its own line and treble the file for no more readability than this.
/// Reading is unaffected either way -- the parser does not care where the
/// newlines are.
fn to_readable_json(db: &Database) -> std::io::Result<String> {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"format\": {},\n", db.format));
    out.push_str("  \"favourites\": ");
    out.push_str(&encode(&db.favourites)?);
    out.push_str(",\n  \"known\": [\n");
    lines(&mut out, &db.known)?;
    out.push_str("  ]\n}\n");
    Ok(out)
}

/// The catalogue as text, the same way, with the cursor in the header.
fn catalogue_json(cat: &Catalogue) -> std::io::Result<String> {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"format\": {},\n", cat.format));
    out.push_str(&format!("  \"cursor\": {},\n", cat.cursor));
    out.push_str("  \"digests\": ");
    out.push_str(&encode(&cat.digests)?);
    out.push_str(",\n  \"games\": [\n");
    lines(&mut out, &cat.games)?;
    out.push_str("  ]\n}\n");
    Ok(out)
}

/// One record to a line, indented and comma-separated.
fn lines<T: serde::Serialize>(out: &mut String, records: &[T]) -> std::io::Result<()> {
    for (i, record) in records.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&encode(record)?);
        if i + 1 < records.len() {
            out.push(',');
        }
        out.push('\n');
    }
    Ok(())
}

fn encode<T: serde::Serialize>(value: &T) -> std::io::Result<String> {
    serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write a store through a temporary file, so a write interrupted half way
/// leaves the previous one rather than half of a new one.
fn write_json(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = path.with_extension("json.partial");
    std::fs::write(&temp, text)?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            Err(e)
        }
    }
}

/// Sixteen bytes as the text the store keys on.
fn hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    /// Two packages that differ only in punctuation get different art.
    ///
    /// The key was the path with every separator flattened to `_`, which
    /// made `A/B.lha` and `A_B.lha` the same file: whichever cover was set
    /// second replaced the first.
    #[test]
    fn art_keys_do_not_collide_on_punctuation() {
        let a = Database::art_key("A/B.lha");
        let b = Database::art_key("A_B.lha");
        assert_ne!(a, b, "two packages share one cover file");
        assert_eq!(a, Database::art_key("A/B.lha"), "and it is stable");
        // Still a name every filesystem takes.
        assert!(a.starts_with("manual-"));
        assert!(
            a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{a} is not a safe file name"
        );
    }

    use super::*;

    #[test]
    fn a_package_name_reduces_to_the_game_it_names() {
        // The four in the test library, and the shapes installers use.
        for (file, want) in [
            ("GoldenAxe_v1.5_0017.lha", "goldenaxe"),
            ("JamesPond2_v2.0_AGA_1354.lha", "jamespond2"),
            ("KingsQuest5_v1.3.lha", "kingsquest5"),
            ("SimCity_v1.0_2193.lha", "simcity"),
            // A version written with a capital, and one with no version.
            ("Lemmings_V2.1_0123.lha", "lemmings"),
            ("Zool.lha", "zool"),
        ] {
            assert_eq!(match_key(&strip_package_name(file)), want, "{file}");
        }
    }

    #[test]
    fn two_spellings_of_a_game_meet_at_the_same_key() {
        // The package is named by an installer and the entry by a
        // cataloguer, so this is the whole job.
        for (package, catalogued) in [
            ("JamesPond2_v2.0_AGA_1354.lha", "James Pond II: Robocod"),
            ("GoldenAxe_v1.5_0017.lha", "Golden Axe"),
            // The trailing single letter, which only meets through the
            // second key each name is also filed under.
            ("KingsQuest5_v1.3.lha", "King's Quest V"),
            ("SimCity_v1.0_2193.lha", "Sim City"),
            ("TheSettlers_v1.2_0456.lha", "Settlers, The"),
            ("Turrican2_v1.0.lha", "Turrican II: The Final Fight"),
            // An installer writes the ampersand a cataloguer writes out.
            ("Utopia&NewWorlds_v1.3.lha", "Utopia and New Worlds"),
        ] {
            let both = |name: &str| {
                let (plain, folded) = match_keys(name);
                let mut all = vec![plain];
                all.extend(folded);
                all
            };
            let from_package = both(&strip_package_name(package));
            let from_catalogue = both(catalogued);
            assert!(
                from_package.iter().any(|k| from_catalogue.contains(k)),
                "{package} {from_package:?} vs {catalogued} {from_catalogue:?}"
            );
        }

        // And two games that are not the same game do not meet: a bare
        // trailing letter is a letter first and a numeral second.
        let ranx = match_key(&strip_package_name("RanX_v1.4.lha"));
        assert_eq!(ranx, match_key("Ranx"), "RanX stopped being Ranx");
    }

    #[test]
    fn words_in_order_is_stricter_than_words_in_any_order() {
        let w = |s: &str| title_words_full(s);
        // The package is the catalogue title plus a word saying which of
        // two games it is.
        assert!(is_subsequence(
            &w("Indiana Jones and the Last Crusade"),
            &w("IndianaJones&TheLastCrusadeAction")
        ));
        // Same words, different game. Order is the whole guard here.
        assert!(!is_subsequence(&w("World Rugby"), &w("RugbyTheWorldCup")));
        // And a title nothing is missing from still matches itself.
        assert!(is_subsequence(&w("Sim City"), &w("SimCity")));
    }

    #[test]
    fn a_loose_match_is_taken_only_when_it_is_close() {
        // The spellings that should meet, and the pair that should not.
        assert!(similarity("rbitwobaseball", "rbi2baseball") >= 70);
        assert_eq!(similarity("simcity", "simcity"), 100);
        assert!(
            similarity("ultimategolf", "ultimatequiz") < MIN_SIMILARITY,
            "two different games were called alike"
        );
        assert_eq!(similarity("", ""), 0);
        assert_eq!(edit_distance(b"kitten", b"sitting"), 3);
        assert_eq!(edit_distance(b"", b"abc"), 3);
        assert_eq!(edit_distance(b"abc", b""), 3);
    }

    #[test]
    fn a_package_finds_a_game_the_key_alone_would_miss() {
        use crate::gamelib::openretro::Record;
        let mut cat = Catalogue::new();
        let mut id = 0;
        let mut add = |cat: &mut Catalogue, name: &str| {
            id += 1;
            cat.apply(&[Record {
                sync_id: id,
                uuid: [id as u8; 16],
                json: Some(format!(r#"{{"game_name":"{name}"}}"#)),
            }]);
        };
        add(&mut cat, "Indiana Jones and the Last Crusade");
        add(&mut cat, "Robin Hood: Legend Quest");
        add(&mut cat, "UMS (Universal Military Simulator)");
        add(&mut cat, "World Rugby");
        add(&mut cat, "War");

        let name = |file: &str| cat.match_file(file).map(|g| g.name.as_str());
        // Words in order, subtitle and all.
        assert_eq!(
            name("IndianaJones&TheLastCrusadeAction_v1.2_1619.lha"),
            Some("Indiana Jones and the Last Crusade")
        );
        assert_eq!(
            name("RobinHoodLegendQuest_v1.1.lha"),
            Some("Robin Hood: Legend Quest")
        );
        // Close enough spelt out.
        assert_eq!(
            name("UniversalMilitarySimulator_v1.0_0753.lha"),
            Some("UMS (Universal Military Simulator)")
        );
        // The two that must not be taken: same words in the wrong order,
        // and a one-word title that is a fragment of everything.
        assert_eq!(name("RugbyTheWorldCup_v1.2_0258.lha"), None);
        assert_eq!(name("UMS2NationsAtWar_v1.0_2137.lha"), None);
    }

    #[test]
    fn a_numeral_that_is_really_a_word_stays_one() {
        // "Mix" and "Civ" are not roman numerals, and a single letter is a
        // word far more often than a number.
        assert_eq!(match_key("Mix"), "mix");
        assert_eq!(match_key("Civilization"), "civilization");
        assert_ne!(match_key("I Game"), "1game");
    }

    #[test]
    fn a_later_page_updates_an_entry_rather_than_repeating_it() {
        use crate::gamelib::openretro::Record;
        let record = |sync_id: u32, uuid: u8, json: Option<&str>| Record {
            sync_id,
            uuid: [uuid; 16],
            json: json.map(str::to_string),
        };
        let mut db = Catalogue::new();

        let added = db.apply(&[
            record(1, 0xAA, Some(r#"{"game_name":"Turrican","year":"1990"}"#)),
            // A variant, with no game name: not a game, not stored.
            record(2, 0xBB, Some(r#"{"variant_name":"Turrican (Disk 1)"}"#)),
        ]);
        assert_eq!(added, 1);
        assert_eq!(db.len(), 1);
        assert_eq!(db.cursor, 2, "the cursor takes every record, game or not");

        // The same entry again, changed: updated in place.
        let changed = db.apply(&[record(
            5,
            0xAA,
            Some(r#"{"game_name":"Turrican","year":"1990","publisher":"Rainbow Arts"}"#),
        )]);
        assert_eq!(changed, 1);
        assert_eq!(db.len(), 1);
        assert_eq!(db.games[0].publisher.as_deref(), Some("Rainbow Arts"));

        // And unchanged: nothing to report, which is what "already up to
        // date" is read from.
        let again = db.apply(&[record(
            6,
            0xAA,
            Some(r#"{"game_name":"Turrican","year":"1990","publisher":"Rainbow Arts"}"#),
        )]);
        assert_eq!(again, 0);

        // A deletion removes it.
        assert_eq!(db.apply(&[record(7, 0xAA, None)]), 1);
        assert!(db.is_empty());
        assert_eq!(db.cursor, 7);
    }

    #[test]
    fn a_favourite_survives_being_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!(
            "copperline-favs-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("db.json");
        let mut db = Database::new();
        assert!(!db.is_favourite("GoldenAxe_v1.5_0017.lha"));
        assert!(db.toggle_favourite("GoldenAxe_v1.5_0017.lha", "Golden Axe"));
        assert!(db.is_favourite("GoldenAxe_v1.5_0017.lha"));
        assert_eq!(db.favourite_count(), 1);
        db.save(&path).expect("saved");

        // Survives the session, without anything in the machine's config.
        let back = Database::load(&path);
        assert!(back.is_favourite("GoldenAxe_v1.5_0017.lha"));
        // Kept per package: a collection holds the same game several
        // times over, and starring one of them marks that one rather than
        // every release of it.
        assert!(!back.is_favourite("GoldenAxe_v2.0_9999.lha"));

        // The title is kept with it, so a favourite whose package has been
        // deleted still has something to show in the list.
        assert_eq!(
            back.favourites().collect::<Vec<_>>(),
            [("GoldenAxe_v1.5_0017.lha", "Golden Axe")]
        );

        // And it can be taken off again, by the tick beside the game or by
        // the one beside the favourite.
        let mut back = back;
        assert!(!back.toggle_favourite("GoldenAxe_v1.5_0017.lha", "Golden Axe"));
        assert_eq!(back.favourite_count(), 0);
        assert!(back.toggle_favourite("GoldenAxe_v1.5_0017.lha", "Golden Axe"));
        back.remove_favourite("GoldenAxe_v1.5_0017.lha");
        assert_eq!(back.favourite_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_store_is_written_a_record_to_a_line() {
        use crate::gamelib::openretro::Record;
        // 676 KiB on one line is unreadable when a game matched wrongly
        // and you want to look at why.
        let mut cat = Catalogue::new();
        for (i, name) in ["Golden Axe", "Sim City", "Turrican"]
            .into_iter()
            .enumerate()
        {
            cat.apply(&[Record {
                sync_id: i as u32 + 1,
                uuid: [i as u8; 16],
                json: Some(format!(r#"{{"game_name":"{name}"}}"#)),
            }]);
        }
        let text = catalogue_json(&cat).expect("serialises");
        assert_eq!(
            text.lines().filter(|l| l.starts_with("    {")).count(),
            3,
            "one game to a line"
        );
        // Still exactly what serde reads back, newlines or not.
        let back: Catalogue = serde_json::from_str(&text).expect("parses");
        assert_eq!(back.games.len(), 3);
        assert_eq!(back.cursor, cat.cursor);

        // And the scanned library the same way.
        let mut db = Database::new();
        db.set_known(
            ["b.lha", "a.lha"]
                .into_iter()
                .map(|file| Known {
                    file: file.to_string(),
                    game: None,
                    manual: false,
                    slave_sha1: None,
                })
                .collect(),
        );
        let text = to_readable_json(&db).expect("serialises");
        assert_eq!(text.lines().filter(|l| l.starts_with("    {")).count(), 2);
    }

    #[test]
    fn a_version_is_kept_and_survives_a_scan() {
        // Not a catalogue field: a sync has no opinion about which release
        // a package is, so a version only ever comes from a person -- and
        // has to survive the scan that would otherwise overwrite it.
        let mut db = Database::new();
        db.set_entry(Known {
            file: "CannonFodder2_v1.11_0104.lha".to_string(),
            game: Some(Game {
                name: "Cannon Fodder 2".to_string(),
                version: Some("CD32 v1.1".to_string()),
                ..Game::default()
            }),
            slave_sha1: Some("aa".to_string()),
            manual: true,
        });
        db.set_known(vec![Known {
            file: "CannonFodder2_v1.11_0104.lha".to_string(),
            game: Some(Game {
                name: "Cannon Fodder 2".to_string(),
                ..Game::default()
            }),
            slave_sha1: Some("aa".to_string()),
            manual: false,
        }]);
        assert_eq!(
            db.match_file("CannonFodder2_v1.11_0104.lha")
                .and_then(|g| g.version.as_deref()),
            Some("CD32 v1.1")
        );

        // And through the store on disk.
        let dir = std::env::temp_dir().join(format!("copperline-version-{}", std::process::id()));
        let at = dir.join("db.json");
        db.save(&at).unwrap();
        assert_eq!(
            Database::load(&at)
                .match_file("CannonFodder2_v1.11_0104.lha")
                .and_then(|g| g.version.as_deref()),
            Some("CD32 v1.1")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_lists_twice_and_a_deleted_one_drops_out() {
        // The same game held as both an .lha and a .zip is a duplicate,
        // which is the person's business rather than ours: it lists twice
        // and both play. Deleting one and pressing Refresh takes that one
        // out and leaves the other alone.
        let mut db = Database::new();
        db.merge_found(vec![
            "GoldenAxe_v1.lha".to_string(),
            "GoldenAxe_v1.zip".to_string(),
        ]);
        assert_eq!(db.len(), 2, "a duplicate should list twice");
        // Both reduce to the same title, which is what makes them look
        // like duplicates in the list -- and share a favourites key, so
        // starring the game stars the game rather than one copy of it.
        assert_eq!(
            match_key(&strip_package_name("GoldenAxe_v1.lha")),
            match_key(&strip_package_name("GoldenAxe_v1.zip"))
        );

        db.merge_found(vec!["GoldenAxe_v1.zip".to_string()]);
        assert_eq!(db.len(), 1);
        assert!(db.entry("GoldenAxe_v1.zip").is_some());
        assert!(
            db.entry("GoldenAxe_v1.lha").is_none(),
            "the deleted one stayed"
        );
    }

    #[test]
    fn a_correction_survives_a_rescan_and_a_rename() {
        let manual = |file: &str, name: &str, sha1: &str| Known {
            file: file.to_string(),
            game: Some(Game {
                name: name.to_string(),
                ..Game::default()
            }),
            slave_sha1: Some(sha1.to_string()),
            manual: true,
        };
        let scanned = |file: &str, name: &str, sha1: &str| Known {
            file: file.to_string(),
            game: Some(Game {
                name: name.to_string(),
                ..Game::default()
            }),
            slave_sha1: Some(sha1.to_string()),
            manual: false,
        };
        let mut db = Database::new();
        db.set_known(vec![manual("Axe_v1.lha", "Golden Axe", "aa")]);

        // A rescan matched it to something else. The correction wins: it
        // was made because the catalogue had it wrong.
        db.set_known(vec![scanned("Axe_v1.lha", "Golden Axe II", "aa")]);
        assert_eq!(
            db.match_file("Axe_v1.lha").map(|g| g.name.as_str()),
            Some("Golden Axe")
        );
        assert!(db.entry("Axe_v1.lha").is_some_and(|k| k.manual));

        // Renamed, and rescanned. Same slave, so the same game: the
        // correction follows the package rather than the name it had.
        db.set_known(vec![scanned("A/GoldenAxe.zip", "Golden Axe II", "aa")]);
        assert_eq!(
            db.match_file("A/GoldenAxe.zip").map(|g| g.name.as_str()),
            Some("Golden Axe")
        );
        // And it is not left behind under the old name as well.
        assert_eq!(db.len(), 1);
        assert!(db.match_file("Axe_v1.lha").is_none());

        // A different package with a different slave is not adopted.
        db.set_known(vec![
            scanned("A/GoldenAxe.zip", "Golden Axe II", "aa"),
            scanned("B/Other.lha", "Something Else", "bb"),
        ]);
        assert_eq!(
            db.match_file("B/Other.lha").map(|g| g.name.as_str()),
            Some("Something Else")
        );
        assert!(db.entry("B/Other.lha").is_some_and(|k| !k.manual));
    }

    #[test]
    fn the_scanned_library_is_sorted_and_found_by_exact_file() {
        // Sorted on the way in, so opening the page is a parse and a
        // binary search rather than a parse and an index build.
        let mut db = Database::new();
        db.set_known(
            [
                ("C/Zool_v1.0.lha", "Zool"),
                ("A/Axe_v1.0.lha", "Golden Axe"),
            ]
            .into_iter()
            .map(|(file, name)| Known {
                file: file.to_string(),
                game: Some(Game {
                    name: name.to_string(),
                    ..Game::default()
                }),
                manual: false,
                slave_sha1: None,
            })
            .collect(),
        );
        assert_eq!(
            db.known()
                .iter()
                .map(|k| k.file.as_str())
                .collect::<Vec<_>>(),
            ["A/Axe_v1.0.lha", "C/Zool_v1.0.lha"]
        );
        assert_eq!(
            db.match_file("A/Axe_v1.0.lha").map(|g| g.name.as_str()),
            Some("Golden Axe")
        );
        // Exact: the scan already did the matching, and two packages of the
        // same name in different folders are two packages.
        assert!(db.match_file("Axe_v1.0.lha").is_none());
        assert!(db.match_file("B/Axe_v1.0.lha").is_none());
    }

    #[test]
    fn a_store_survives_a_round_trip_and_refuses_a_foreign_one() {
        use crate::gamelib::openretro::Record;
        let dir = std::env::temp_dir().join(format!(
            "copperline-gamedb-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        // The catalogue: the cursor comes back, and the index with it.
        let at = dir.join("openretro.json");
        let mut cat = Catalogue::new();
        cat.apply(&[Record {
            sync_id: 3,
            uuid: [0xCC; 16],
            json: Some(r#"{"game_name":"Sim City","front_sha1":"abc"}"#.into()),
        }]);
        cat.save(&at).expect("saved");
        let back = Catalogue::load(&at);
        assert_eq!(back.cursor, 3);
        assert_eq!(back.len(), 1);
        // The index is rebuilt on load, not stored, so matching works.
        assert_eq!(
            back.match_file("SimCity_v1.0_2193.lha")
                .map(|g| g.name.as_str()),
            Some("Sim City")
        );
        assert!(!at.with_extension("json.partial").exists());

        // A file from another format is not read as this one, and neither
        // is anything that is not the file at all.
        std::fs::write(&at, r#"{"format":999,"cursor":42,"games":[]}"#).unwrap();
        assert_eq!(Catalogue::load(&at).cursor, 0, "a foreign format was read");
        std::fs::write(&at, "not json").unwrap();
        assert_eq!(Catalogue::load(&at).cursor, 0);
        assert_eq!(Catalogue::load(&dir.join("nothing.json")).cursor, 0);

        // And the scanned library, which carries the favourites.
        let path = dir.join("db.json");
        let mut db = Database::new();
        db.set_known(vec![Known {
            file: "SimCity_v1.0_2193.lha".to_string(),
            game: Some(Game {
                name: "Sim City".to_string(),
                front_sha1: Some("abc".to_string()),
                ..Game::default()
            }),
            manual: false,
            slave_sha1: None,
        }]);
        db.toggle_favourite("SimCity_v1.0_2193.lha", "Sim City");
        db.save(&path).expect("saved");
        let back = Database::load(&path);
        assert_eq!(back.len(), 1);
        assert!(back.is_favourite("SimCity_v1.0_2193.lha"));
        assert_eq!(
            back.match_file("SimCity_v1.0_2193.lha")
                .and_then(|g| g.front_sha1.as_deref()),
            Some("abc")
        );
        std::fs::write(&path, r#"{"format":999,"known":[]}"#).unwrap();
        assert!(Database::load(&path).is_empty());

        // A store somebody hand-edited out of order still matches: it is
        // put right on the way in rather than quietly stopping working.
        std::fs::write(
            &path,
            r#"{"format":1,"known":[{"file":"z.lha"},{"file":"a.lha"}]}"#,
        )
        .unwrap();
        let fixed = Database::load(&path);
        assert_eq!(
            fixed
                .known()
                .iter()
                .map(|k| k.file.as_str())
                .collect::<Vec<_>>(),
            ["a.lha", "z.lha"]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
