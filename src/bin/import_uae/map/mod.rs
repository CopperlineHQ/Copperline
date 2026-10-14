//! Per-source-format mapping from parsed `key=value` entries to a
//! Copperline TOML document. Each dialect gets its own module: WinUAE and
//! Amiberry share a vocabulary closely enough to share one mapper (see
//! `winuae::map`, used for both), but FS-UAE's key names, units, and its
//! `amiga_model` preset system are different enough to need a real second
//! implementation, not a lookup-table diff.

pub mod fsuae;
pub mod winuae;

use crate::report::ImportReport;
use toml_edit::{DocumentMut, Item, Table};

/// Which dialect a source file is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    WinUae,
    Amiberry,
    FsUae,
}

impl SourceFormat {
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "winuae" => Some(Self::WinUae),
            "amiberry" => Some(Self::Amiberry),
            "fsuae" => Some(Self::FsUae),
            _ => None,
        }
    }
}

pub struct MapOutcome {
    pub doc: DocumentMut,
    pub report: ImportReport,
}

/// `source` is the config file being read. It is used only to resolve
/// relative image paths well enough to *describe* them in the report (a
/// drive's size, say); nothing it finds or fails to find changes the
/// emitted config, so a miss costs only a vaguer warning.
pub fn map(
    format: SourceFormat,
    entries: &[crate::parse::Entry],
    source: &std::path::Path,
) -> MapOutcome {
    match format {
        SourceFormat::WinUae | SourceFormat::Amiberry => winuae::map(entries, source),
        SourceFormat::FsUae => fsuae::map(entries),
    }
}

/// Find a config-relative file the way the emulator that wrote the config
/// would. Amiberry keeps its configs in `conf/` or `Configurations/` under
/// an install root and resolves bare names against that root's own media
/// folders, so the config's directory alone is not enough. Best-effort and
/// diagnostic only -- see [`map`].
pub(crate) fn resolve_media_path(
    source: &std::path::Path,
    path: &str,
) -> Option<std::path::PathBuf> {
    let given = std::path::Path::new(path);
    if given.is_absolute() {
        return given.is_file().then(|| given.to_path_buf());
    }
    let dir = source.parent()?;
    let root = dir.parent();
    let mut candidates: Vec<std::path::PathBuf> = vec![given.to_path_buf(), dir.join(given)];
    if let Some(root) = root {
        candidates.push(root.join(given));
        for media in ["Harddrives", "HardDrives", "hardfiles", "Floppies"] {
            candidates.push(root.join(media).join(given));
        }
    }
    candidates.into_iter().find(|c| c.is_file())
}

/// Get (creating if absent) the `[a.b.c]` table at `path`, e.g.
/// `["floppy", "df0"]` for `[floppy.df0]`.
pub(crate) fn table<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> &'a mut Table {
    let mut current = doc.as_table_mut();
    for (depth, segment) in path.iter().enumerate() {
        let entry = current
            .entry(segment)
            .or_insert_with(|| Item::Table(Table::new()));
        current = entry.as_table_mut().expect("path segment is a table");
        // A table that only exists to hold `[a.b]` is implicit: written
        // out it would be a bare `[a]` header with nothing under it.
        // Anything that gains keys of its own turns explicit again below.
        if depth + 1 < path.len() && current.is_empty() {
            current.set_implicit(true);
        }
    }
    current
}

/// Set `doc[path...][key] = value` (a plain string), creating intermediate
/// tables as needed.
pub(crate) fn set_str(doc: &mut DocumentMut, path: &[&str], key: &str, value: &str) {
    table(doc, path)[key] = toml_edit::value(value);
}

/// What one floppy drive's sound setting in the source config asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DriveSound {
    /// A silent drive.
    Off,
    /// The emulator's own built-in sounds; Copperline's are synthesized.
    BuiltIn,
    /// A WinUAE-format sample set (`drive_click_NAME.wav` and friends).
    Set(String),
}

/// Map per-drive sound settings, given as `(key, value, choice)` in drive
/// order for the drives the source mentions. Copperline makes one choice
/// for every drive, so the first drive's decides and a drive asking for
/// something else is flagged. A named set is the user's own files, which
/// the import cannot carry over, so it is flagged for them to copy.
pub(crate) fn apply_drive_sounds(
    doc: &mut DocumentMut,
    report: &mut ImportReport,
    choices: &[(String, String, DriveSound)],
) {
    let Some((key, value, choice)) = choices.first() else {
        return;
    };
    match choice {
        DriveSound::Off => table(doc, &["audio"])["floppy_sounds"] = toml_edit::value(false),
        DriveSound::BuiltIn => {}
        DriveSound::Set(name) => {
            set_str(doc, &["audio"], "floppy_sounds_set", name);
            report.approximated(
                key,
                value,
                format!(
                    "copy the set's drive_*_{name}.wav files into Copperline's \"Floppy \
                     Sounds\" folder (Documents/Copperline/Floppy Sounds), or set \
                     floppy_sounds_set to the path of one of them"
                ),
            );
        }
    }
    for (other_key, other_value, other) in &choices[1..] {
        if other != choice {
            report.unsupported(
                other_key,
                other_value,
                format!(
                    "Copperline makes one drive sound choice for every drive; {key}'s was used"
                ),
            );
        }
    }
}

/// Copperline's chip RAM ceiling for the common ECS/AGA case (OCS caps
/// lower still, at 512K, which validation catches separately). A source
/// config claiming more chip RAM than real hardware ever carried is
/// clamped down rather than passed through to fail validation with no
/// path forward; the caller folds the returned note into its own comment
/// so the original value stays visible.
pub(crate) fn clamp_chip_mb(size: &str) -> (String, Option<String>) {
    match size.strip_suffix('M').and_then(|n| n.parse::<u64>().ok()) {
        Some(mb) if mb > 2 => (
            "2M".to_string(),
            Some(format!(
                "source specified {size} of chip RAM; clamped to Copperline's 2M ceiling"
            )),
        ),
        _ => (size.to_string(), None),
    }
}

/// Attach a comment directly above `doc[path...][key]`, for a value that
/// was set but is only an approximation of the source setting. Comments
/// live in the *key's* leading decor (the text before the key token on its
/// own line) -- putting them on the value's decor instead would insert a
/// `#...` between `=` and the value, which is not valid TOML on one line.
pub(crate) fn annotate(doc: &mut DocumentMut, path: &[&str], key: &str, comment: &str) {
    let t = table(doc, path);
    if let Some(mut key_mut) = t.key_mut(key) {
        key_mut
            .leaf_decor_mut()
            .set_prefix(format!("# {comment}\n"));
    }
}
