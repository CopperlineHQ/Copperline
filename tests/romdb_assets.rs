// SPDX-License-Identifier: GPL-3.0-or-later

//! Asset-gated checks of the Kickstart ROM identification table
//! (`src/romdb.rs`) against real ROM dumps. The unit tests in that module
//! cover the table's shape and the normalisation with synthetic buffers;
//! this test proves the transcribed checksums actually name the images a
//! real collection holds, in the file forms they come in (256 KiB parts,
//! 512 KiB images, an A1000 bootstrap echoed across its window).
//!
//! ROM images are local assets and are never committed, so every file is
//! optional: absent ones are skipped and the test passes.

use copperline::romdb;

/// The integration-test asset directory (see `tests/README.md`):
/// `COPPERLINE_TEST_ASSETS`, else `test-assets/` under the repo root,
/// else the repo root itself.
fn asset_dir() -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match std::env::var_os("COPPERLINE_TEST_ASSETS") {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let d = root.join("test-assets");
            if d.is_dir() {
                d
            } else {
                root
            }
        }
    }
}

/// One asset by name, or `None` when the collection does not hold it.
fn asset(name: &str) -> Option<std::path::PathBuf> {
    let path = asset_dir().join(name);
    path.is_file().then_some(path)
}

/// Local ROM dumps and what the table must call them. The file names are the
/// ones the other ignored suites use (see `tests/README.md`); a collection
/// keeping the same images under other names simply skips these rows.
const KNOWN_ROMS: [(&str, &str); 9] = [
    ("kick13.rom", "Kickstart 1.3 (34.5) A500/A1000/A2000"),
    ("KICK13.ROM", "Kickstart 1.3 (34.5) A500/A1000/A2000"),
    ("kickstart205.rom", "Kickstart 2.05 (37.299) A600"),
    ("KICK31.ROM", "Kickstart 3.1 (40.63) A500/A600/A2000"),
    (
        "Kickstart v3.1 r40.68 (1993)(Commodore)(A1200)[!].rom",
        "Kickstart 3.1 (40.68) A1200",
    ),
    (
        "Kickstart v3.1 r40.68 (1993)(Commodore)(A4000).rom",
        "Kickstart 3.1 (40.68) A4000",
    ),
    (
        "Kickstart v3.1 r40.060 (1993-05)(Commodore)(CD32)[!].rom",
        "Kickstart 3.1 (40.60) CD32",
    ),
    (
        "CD32 Extended-ROM r40.60 (1993)(Commodore)(CD32).rom",
        "CD32 extended ROM (40.60)",
    ),
    (
        "Amiga 1000 ROM Bootstrap (1985)(Commodore)(A1000)[!].rom",
        "A1000 bootstrap ROM",
    ),
];

#[test]
#[ignore = "needs local Kickstart ROM images (see tests/README.md)"]
fn real_rom_dumps_identify_as_their_kickstart_version() {
    let mut checked = 0;
    for (name, expected) in KNOWN_ROMS {
        let Some(path) = asset(name) else {
            continue;
        };
        let data = std::fs::read(&path).expect("ROM reads");
        let identified = romdb::describe(&data);
        assert_eq!(
            identified.map(|id| id.label()),
            Some(expected),
            "{}: unexpected identification",
            path.display()
        );
        eprintln!("{name}: {expected}");
        checked += 1;
    }
    if checked == 0 {
        eprintln!("skipping: no known Kickstart ROM in the asset directory");
    }
}

/// Every ROM-shaped file in the asset directory, named. Not an assertion
/// about coverage -- a collection holds plenty of images the table does not
/// carry (AROS, DiagROM, expansion-board ROMs) -- but a check that reading a
/// real directory of images never panics and that the ones that do identify
/// are self-consistent: the reported entry's size is the size the file
/// normalises to.
#[test]
#[ignore = "needs a local ROM collection (see tests/README.md)"]
fn scanning_a_rom_collection_names_what_it_can() {
    let Ok(entries) = std::fs::read_dir(asset_dir()) else {
        eprintln!("skipping: asset directory unreadable");
        return;
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("rom") || e.eq_ignore_ascii_case("bin"))
        })
        .collect();
    paths.sort();
    for path in paths {
        let Some(identified) = romdb::describe_file(&path) else {
            eprintln!("{}: not identified", path.display());
            continue;
        };
        eprintln!("{}: {}", path.display(), identified.label());
        if let romdb::Identified::Known(entry) = identified {
            let len = std::fs::metadata(&path).expect("metadata").len() as usize;
            assert!(
                len.is_multiple_of(entry.size),
                "{}: identified as {} ({} bytes) but the file is {len} bytes",
                path.display(),
                entry.label,
                entry.size
            );
        }
    }
}
