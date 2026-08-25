//! Tracks every source key that didn't cleanly become a Copperline setting,
//! so the converter never silently drops something. Every mapper consults
//! this as it walks the source entries: an entry it doesn't recognize, or
//! maps only approximately, is flagged here instead of just being skipped.

/// How a source key relates to the generated config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Translated, but the semantics differ enough that the result should
    /// be checked by hand (e.g. WinUAE's true JIT vs Copperline's
    /// batch/trace `cpu.jit`, or a floppy-speed value that doesn't map to
    /// an exact Copperline equivalent).
    Approximated,
    /// No Copperline equivalent exists at all (e.g. per-board RTG/expansion
    /// config, warp/statefile host paths, input device autodetection).
    Unsupported,
}

/// One flagged source key: what it was, why it's flagged, and (for
/// `Approximated`) what it became.
#[derive(Debug, Clone)]
pub struct FlaggedKey {
    pub source_key: String,
    pub source_value: String,
    pub bucket: Bucket,
    pub note: String,
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub flagged: Vec<FlaggedKey>,
    /// Remarks that belong to no particular source key -- typically about
    /// something the config never said, which by definition has no key to
    /// hang off. Kept apart from `flagged` so the two are not conflated:
    /// these are not settings that failed to translate.
    pub notes: Vec<String>,
}

impl ImportReport {
    pub fn approximated(&mut self, key: &str, value: &str, note: impl Into<String>) {
        self.flagged.push(FlaggedKey {
            source_key: key.to_string(),
            source_value: value.to_string(),
            bucket: Bucket::Approximated,
            note: note.into(),
        });
    }

    /// Record something worth saying that no source key accounts for.
    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    pub fn unsupported(&mut self, key: &str, value: &str, note: impl Into<String>) {
        self.flagged.push(FlaggedKey {
            source_key: key.to_string(),
            source_value: value.to_string(),
            bucket: Bucket::Unsupported,
            note: note.into(),
        });
    }

    /// A trailing TOML comment block listing every flagged key, grouped by
    /// bucket, for the tail of the generated file. Per-key inline comments
    /// (dropped next to the section they'd conceptually belong to) are the
    /// mapper's job, via `toml_edit` decor -- this covers the keys with no
    /// natural Copperline home to sit next to.
    pub fn trailer_comment(&self) -> String {
        if self.flagged.is_empty() && self.notes.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for note in &self.notes {
            out.push_str(&format!("# Note: {note}\n"));
        }
        if self.flagged.is_empty() {
            return out;
        }
        if !self.notes.is_empty() {
            out.push('\n');
        }
        out.push_str("# --- Settings from the source config that were not translated ---\n");
        for bucket in [Bucket::Approximated, Bucket::Unsupported] {
            let keys: Vec<_> = self.flagged.iter().filter(|f| f.bucket == bucket).collect();
            if keys.is_empty() {
                continue;
            }
            let heading = match bucket {
                Bucket::Approximated => "# Approximated (semantics differ -- verify by hand):",
                Bucket::Unsupported => "# Unsupported (no Copperline equivalent):",
            };
            out.push_str(heading);
            out.push('\n');
            for f in keys {
                out.push_str(&format!(
                    "#   {} = {}  ({})\n",
                    f.source_key, f.source_value, f.note
                ));
            }
        }
        out
    }
}
