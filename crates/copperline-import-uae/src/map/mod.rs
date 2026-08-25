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

pub fn map(format: SourceFormat, entries: &[crate::parse::Entry]) -> MapOutcome {
    match format {
        SourceFormat::WinUae | SourceFormat::Amiberry => winuae::map(entries),
        SourceFormat::FsUae => fsuae::map(entries),
    }
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
