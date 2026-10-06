// SPDX-License-Identifier: GPL-3.0-or-later

//! Window-title templates. Formatting is host presentation only.

use anyhow::{bail, Result};

pub const DEFAULT_TEMPLATE: &str = "{app} {version} · {machine} · {rom}";

pub struct TitleValues<'a> {
    pub app: &'a str,
    pub version: &'a str,
    pub machine: &'a str,
    pub rom: &'a str,
    pub hash: &'a str,
}

/// Expand a small, deliberately fixed set of placeholders. Double braces
/// quote a literal brace; misspellings fail config validation at startup.
pub fn render(template: &str, values: &TitleValues<'_>) -> Result<String> {
    let mut out = String::with_capacity(template.len() + 48);
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '{' => {
                let mut key = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) if c != '{' => key.push(c),
                        _ => bail!("unclosed or nested window-title placeholder"),
                    }
                }
                out.push_str(match key.as_str() {
                    "app" => values.app,
                    "version" => values.version,
                    "machine" | "m" => values.machine,
                    "rom" | "r" => values.rom,
                    "hash" => values.hash,
                    _ => bail!("unknown window-title placeholder {{{key}}}"),
                });
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '}' => bail!("unmatched }} in window-title template"),
            c => out.push(c),
        }
    }
    Ok(out)
}

pub fn validate(template: &str) -> Result<()> {
    if template.is_empty() || template.len() > 256 || template.chars().any(char::is_control) {
        bail!("[display] title must be 1-256 characters with no control characters");
    }
    render(
        template,
        &TitleValues {
            app: "Copperline",
            version: "1.0",
            machine: "A1200",
            rom: "Kickstart 3.1",
            hash: "12345678",
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_aliases_and_escaped_braces() {
        let values = TitleValues {
            app: "Copperline",
            version: "1.0",
            machine: "A1200",
            rom: "Kickstart 3.1",
            hash: "12345678",
        };
        assert_eq!(
            render("{{test}} {m}: {r} [{hash}]", &values).unwrap(),
            "{test} A1200: Kickstart 3.1 [12345678]"
        );
        assert!(validate("{missing}").is_err());
        assert!(validate("{rom").is_err());
    }
}
