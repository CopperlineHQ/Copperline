// SPDX-License-Identifier: GPL-3.0-or-later

//! Custom-chip register documentation shared by every debugger surface.
//! The source of truth is the ASCII Markdown directory under `docs/reference`;
//! build.rs compiles it into this read-only table.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRegisterDoc {
    pub offset: u16,
    pub name: &'static str,
    pub access: &'static str,
    pub chipset: &'static str,
    pub summary: &'static str,
    pub markdown: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/custom_register_docs.rs"));

pub fn all() -> &'static [CustomRegisterDoc] {
    CUSTOM_REGISTER_DOCS
}

pub fn by_offset(offset: u16) -> Option<&'static CustomRegisterDoc> {
    CUSTOM_REGISTER_DOCS
        .binary_search_by_key(&(offset & 0x1fe), |doc| doc.offset)
        .ok()
        .map(|index| &CUSTOM_REGISTER_DOCS[index])
}

pub fn by_name(name: &str) -> Option<&'static CustomRegisterDoc> {
    CUSTOM_REGISTER_DOCS
        .iter()
        .find(|doc| doc.name.eq_ignore_ascii_case(name.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_docs_are_sorted_complete_and_ascii() {
        assert!(all().windows(2).all(|pair| pair[0].offset < pair[1].offset));
        let named_offsets: Vec<u16> = (0..0x200)
            .step_by(2)
            .filter(|offset| !crate::debugger::custom_reg_name(*offset).starts_with('$'))
            .collect();
        assert_eq!(all().len(), named_offsets.len());
        assert!(named_offsets
            .iter()
            .all(|offset| by_offset(*offset).is_some()));
        for doc in all() {
            assert_eq!(crate::debugger::custom_reg_name(doc.offset), doc.name);
            assert!(doc.markdown.is_ascii());
            assert!(!doc.summary.is_empty());
            assert!(doc.markdown.contains("\n## Bitfields\n"));
            assert!(matches!(doc.access, "read" | "write" | "read/write"));
            assert!(!doc.chipset.is_empty());
        }
        assert_eq!(by_name("dmacon").unwrap().offset, 0x096);
    }
}
