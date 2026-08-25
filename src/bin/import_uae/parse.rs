//! Shared reader for the flat `key=value` config dialects used by WinUAE,
//! Amiberry (a WinUAE fork; same file format) and FS-UAE. All three write
//! one setting per line with no section headers, so a single tokenizer
//! covers every source format; only the *mapping* from key to Copperline
//! setting differs per dialect (see `crate::map`).

/// One `key=value` line from a source config, in file order. Duplicate keys
/// are kept as separate entries (some dialects use repeated `db2N=` style
/// keys for indexed sub-configs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
}

/// Parse WinUAE/Amiberry/FS-UAE style config text: `key=value` or
/// `key = value` per line, `#` and `;` comment lines, blank lines ignored.
/// Unparseable lines (no `=`) are skipped rather than erroring -- a
/// best-effort import should degrade to "didn't recognize this line", not
/// abort on the first stray comment style a real-world file throws at it.
pub fn parse(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some(Entry {
                key: key.trim().to_string(),
                value: value.trim().to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_value_lines() {
        let text = "# a comment\nchipset=aga\n\ncpu_type = 68020\n; also a comment\n";
        let entries = parse(text);
        assert_eq!(
            entries,
            vec![
                Entry {
                    key: "chipset".into(),
                    value: "aga".into()
                },
                Entry {
                    key: "cpu_type".into(),
                    value: "68020".into()
                },
            ]
        );
    }

    #[test]
    fn skips_lines_with_no_equals() {
        let entries = parse("not a valid line\nchipset=aga\n");
        assert_eq!(entries.len(), 1);
    }
}
