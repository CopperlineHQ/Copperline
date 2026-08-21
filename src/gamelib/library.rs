// SPDX-License-Identifier: GPL-3.0-or-later

//! The list the Library page shows: the packages in the game folder, each
//! with whatever the last scan resolved for it.
//!
//! It is built from the store and never from the disk: a collection of
//! several thousand packages filed a letter deep is a directory walk long
//! enough to be felt, and nobody asked for one by clicking a tab. Refresh
//! reads the folder, puts what it finds in the store, and the list follows
//! from that -- so the same list comes back every time the page is opened,
//! and comes back after a restart.
//!
//! With no game library set there is no list: the page says so and says
//! where to set one. It used to stand in the folder holding the chosen
//! game, which read as Clear doing nothing -- emptying the setting left
//! the list full of whatever sat beside the launch game.

use std::path::{Path, PathBuf};

use super::db::{Database, Game};

/// One package on disk, and the game it turned out to be.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The archive itself.
    pub path: PathBuf,
    /// Where it sits under the game folder, which is how the store keys
    /// it: two `Zool_v1.0.lha` filed under different letters are two
    /// packages, and the bare name cannot tell them apart.
    pub relative: String,
    /// The file name without its extension, which is what a person named
    /// it. Shown as the title when the scan could not name the package,
    /// and as the version where a named game is held more than once --
    /// `.lha` against `.zip` is how it was packed, not which release it
    /// is, so the extension is no part of either answer.
    pub file_name: String,
    /// What the database says, when the name matched an entry.
    pub game: Option<Game>,
    /// Whether something else in the list is shown under the same title.
    ///
    /// Collections carry a game several times over --
    /// `CannonFodder2_v1.11_0104`, `_v1.12_Fr_2578`, `_v1.1_De_0241` --
    /// and every one matches the same catalogue entry, so the list shows a
    /// run of rows all reading "Cannon Fodder 2" with nothing to tell them
    /// apart. Where that happens to a *named* game the page offers a
    /// version as well; two packages the scan could not name are two rows
    /// that say nothing already, and a file name under them adds nothing.
    pub duplicated: bool,
}

impl Entry {
    /// What to call it: the catalogued name where there is one, and the
    /// file's own otherwise.
    pub fn title(&self) -> &str {
        match &self.game {
            Some(game) => &game.name,
            None => &self.file_name,
        }
    }
}

/// The packages in a folder, in the order the list shows them.
#[derive(Debug, Default, Clone)]
pub struct Library {
    /// The folder the entries came from, so a rescan can be skipped when
    /// nothing has moved.
    folder: Option<PathBuf>,
    entries: Vec<Entry>,
}

impl Library {
    /// Read a folder of packages, and match each against the database.
    ///
    /// Sorted by the name shown rather than by file name, so a list that
    /// mixes catalogued and uncatalogued games still reads alphabetically.
    ///
    /// Searched all the way down, since a collection is usually filed by
    /// letter or by genre rather than left flat -- but only so far, and
    /// never through a symbolic link. A library pointed at a home
    /// directory, or at a tree that links back into itself, must not walk
    /// the disk or spin forever.
    /// The list as the store has it.
    ///
    /// The store is the only thing the list is ever built from, and
    /// [`Database::merge_found`] is the only thing that reads the folder.
    /// One source means the page shows the same thing every time it is
    /// opened, without walking a collection of several thousand packages
    /// to find out what that is.
    ///
    /// Sorted by the name shown rather than by file name, so a list that
    /// mixes catalogued and uncatalogued games still reads alphabetically.
    pub fn known(folder: &Path, db: &Database) -> Library {
        Library::of(
            folder,
            db.known().iter().map(|known| known.file.clone()),
            db,
        )
    }

    fn of(folder: &Path, files: impl Iterator<Item = String>, db: &Database) -> Library {
        let mut entries: Vec<Entry> = files
            .map(|relative| {
                let base = relative.rsplit(['/', '\\']).next().unwrap_or(&relative);
                Entry {
                    game: db.match_file(&relative).cloned(),
                    file_name: crate::package::stem_of(base).to_string(),
                    path: crate::package::under(folder, &relative),
                    relative,
                    duplicated: false,
                }
            })
            .collect();
        entries.sort_by_key(|entry| sort_key(entry.title()));
        // Sorted by what is shown, so anything sharing a title is adjacent
        // and one pass settles it.
        for i in 0..entries.len() {
            let same = |a: usize, b: usize| entries[a].title() == entries[b].title();
            let before = i > 0 && same(i - 1, i);
            let after = i + 1 < entries.len() && same(i + 1, i);
            entries[i].duplicated = before || after;
        }
        Library {
            folder: Some(folder.to_path_buf()),
            entries,
        }
    }

    /// Whether this list already covers `folder`, so a redraw need not
    /// read the directory again.
    /// A library of made-up entries, for tests that need a list of a
    /// given length without a folder of archives behind it.
    #[cfg(test)]
    pub(crate) fn of_titles(titles: impl IntoIterator<Item = String>) -> Library {
        Library {
            folder: None,
            entries: titles
                .into_iter()
                .map(|title| Entry {
                    path: PathBuf::from(&title),
                    relative: title.clone(),
                    file_name: title,
                    game: None,
                    duplicated: false,
                })
                .collect(),
        }
    }

    pub fn covers(&self, folder: &Path) -> bool {
        self.folder.as_deref() == Some(folder)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Where a package sits in the list, so the game being played can be
    /// shown as the chosen one.
    pub fn position(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }
}

/// A title as the list orders it: digits before letters, case ignored.
///
/// Sorting the text directly would put "1942" after "Zool" on any machine
/// where the locale says so, and would separate "sim city" from "Sim
/// City". Both are the same shelf to a person looking for a game.
fn sort_key(title: &str) -> (u8, String) {
    let folded = title.to_lowercase();
    let group = match folded.chars().next() {
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_alphabetic() => 1,
        // Anything else -- brackets, a leading dot -- after the words
        // rather than mixed into them.
        _ => 2,
    };
    (group, folded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let at = std::env::temp_dir().join(format!(
            "copperline-library-{}-{name}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&at).unwrap();
        at
    }

    /// What Refresh does: read the folder into the store, then list it.
    fn refreshed(dir: &Path, db: &mut Database) -> Library {
        db.merge_found(crate::gamelib::scan::packages(dir));
        Library::known(dir, db)
    }

    #[test]
    fn a_scan_finds_the_packages_and_leaves_everything_else() {
        let dir = scratch("scan");
        for name in [
            "GoldenAxe_v1.5_0017.lha",
            "SimCity_v1.0_2193.lha",
            "notes.txt",
            "Screenshot.png",
            // Case is the file system's business, not the list's.
            "Zool_v1.0.LHA",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let library = refreshed(&dir, &mut Database::new());
        let titles: Vec<&str> = library.entries().iter().map(Entry::title).collect();
        assert_eq!(
            titles,
            ["GoldenAxe_v1.5_0017", "SimCity_v1.0_2193", "Zool_v1.0"]
        );
        assert!(library.covers(&dir));
        assert!(!library.covers(Path::new("/somewhere/else")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_matched_game_is_listed_under_its_catalogued_name() {
        use crate::gamelib::db::Known;
        let dir = scratch("named");
        std::fs::write(dir.join("JamesPond2_v2.0_AGA_1354.lha"), b"x").unwrap();
        std::fs::write(dir.join("Unknown_v1.0.lha"), b"x").unwrap();

        let mut db = Database::new();
        db.set_known(vec![Known {
            file: "JamesPond2_v2.0_AGA_1354.lha".to_string(),
            game: Some(Game {
                name: "James Pond 2: Codename RoboCod".to_string(),
                year: Some("1991".to_string()),
                ..Game::default()
            }),
            manual: false,
            slave_sha1: None,
        }]);

        let library = refreshed(&dir, &mut db);
        // Sorted by what is shown, so the catalogued name decides where it
        // sits rather than the file name it arrived under.
        let titles: Vec<&str> = library.entries().iter().map(Entry::title).collect();
        assert_eq!(titles, ["James Pond 2: Codename RoboCod", "Unknown_v1.0"]);
        assert_eq!(
            library.entries()[0]
                .game
                .as_ref()
                .and_then(|g| g.year.as_deref()),
            Some("1991")
        );
        assert!(library.entries()[1].game.is_none());

        // And a package can be found by path, which is how the game being
        // played stays the chosen one.
        let at = library.position(&dir.join("Unknown_v1.0.lha"));
        assert_eq!(at, Some(1));
        assert_eq!(library.position(Path::new("/nowhere.lha")), None);

        // And the same list without reading the folder again, which is
        // what opening the page does: the store is the only thing it is
        // built from, so what Refresh found is what comes back.
        let listed = Library::known(&dir, &db);
        let titles: Vec<&str> = listed.entries().iter().map(Entry::title).collect();
        assert_eq!(titles, ["James Pond 2: Codename RoboCod", "Unknown_v1.0"]);
        assert_eq!(
            listed.entries()[0].path,
            dir.join("JamesPond2_v2.0_AGA_1354.lha")
        );
        // Reading the folder again does not lose the metadata: that is
        // work a scan spent minutes on.
        let again = refreshed(&dir, &mut db);
        assert_eq!(
            again.entries()[0].game.as_ref().map(|g| g.name.as_str()),
            Some("James Pond 2: Codename RoboCod")
        );

        // A package that has gone drops out; one that appears arrives
        // unresolved.
        std::fs::remove_file(dir.join("Unknown_v1.0.lha")).unwrap();
        std::fs::write(dir.join("Zool_v1.0.lha"), b"x").unwrap();
        let after = refreshed(&dir, &mut db);
        let titles: Vec<&str> = after.entries().iter().map(Entry::title).collect();
        assert_eq!(titles, ["James Pond 2: Codename RoboCod", "Zool_v1.0"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_collection_is_searched_all_the_way_down() {
        // Libraries are filed by letter or by genre rather than left flat.
        let dir = scratch("deep");
        for sub in ["A", "B/Platformers", "C/1/2/3"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("A/Zool_v1.0.lha"), b"x").unwrap();
        std::fs::write(dir.join("B/Platformers/JamesPond2_v2.0.lha"), b"x").unwrap();
        std::fs::write(dir.join("C/1/2/3/SimCity_v1.0.lha"), b"x").unwrap();
        std::fs::write(dir.join("B/notes.txt"), b"x").unwrap();

        let library = refreshed(&dir, &mut Database::new());
        let titles: Vec<&str> = library.entries().iter().map(Entry::title).collect();
        assert_eq!(titles, ["JamesPond2_v2.0", "SimCity_v1.0", "Zool_v1.0"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_search_does_not_follow_a_link_out_of_the_library() {
        // A link pointing at a parent is a loop, and a link pointing
        // somewhere else is a search of somebody's whole disk. Neither is
        // what "list my games" asked for.
        let dir = scratch("links");
        std::fs::create_dir_all(dir.join("games")).unwrap();
        std::fs::write(dir.join("games/Zool_v1.0.lha"), b"x").unwrap();
        // Outside the library, and reachable only through the link.
        let outside = scratch("outside");
        std::fs::write(outside.join("Elsewhere_v1.0.lha"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("games/away")).unwrap();
        // And a loop back to the top.
        std::os::unix::fs::symlink(&dir, dir.join("games/round")).unwrap();

        let library = refreshed(&dir, &mut Database::new());
        let titles: Vec<&str> = library.entries().iter().map(Entry::title).collect();
        assert_eq!(titles, ["Zool_v1.0"], "the search left the library");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_missing_folder_is_an_empty_list_rather_than_a_failure() {
        // Nothing has been chosen yet, or the folder went away: the page
        // shows nothing, which is what it would show anyway.
        let library = refreshed(Path::new("/no/such/folder"), &mut Database::new());
        assert!(library.is_empty());
    }
}
