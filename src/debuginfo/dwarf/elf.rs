// SPDX-License-Identifier: GPL-3.0-or-later

//! Materialize relocatable ELF DWARF in one unambiguous address space.
//! These addresses are only for debug lookup; LoadSeg still supplies the
//! runtime base of each hunk, and no guest bytes are changed here.

use super::{LinkMap, LinkSegment};
use object::{
    Object as _, ObjectSection as _, ObjectSymbol as _, RelocationEncoding, RelocationKind,
    RelocationTarget, SectionIndex, SymbolSection,
};
use std::collections::HashMap;

pub(super) struct Sections {
    pub link: LinkMap,
    bases: HashMap<SectionIndex, u64>,
}

impl Sections {
    pub fn new(file: &object::File<'_>) -> Result<Self, String> {
        let relocatable = file.kind() == object::ObjectKind::Relocatable;
        let mut link = LinkMap::default();
        let mut bases = HashMap::new();
        let mut next = 0u64;
        for section in file.sections() {
            let alloc = matches!(section.flags(), object::SectionFlags::Elf { sh_flags, .. }
                if (sh_flags & object::elf::SHF_ALLOC).0 != 0);
            let mut addr = if relocatable { 0 } else { section.address() };
            if alloc && section.size() != 0 {
                if relocatable {
                    // Leave a gap so a function's exclusive high_pc cannot
                    // alias the beginning of another hunk. All m68k DWARF
                    // addresses must still fit in four bytes.
                    addr = next;
                    let end = addr
                        .checked_add(section.size())
                        .filter(|&end| end <= u64::from(u32::MAX))
                        .ok_or("ELF debug address space exceeds 32 bits")?;
                    next = (end + 4) & !3;
                }
                link.segments.push(LinkSegment {
                    addr,
                    size: section.size(),
                    hunk: link.segments.len() as u32,
                });
            }
            // References between debug sections are section offsets, so
            // their base is zero even when code/data have synthetic bases.
            bases.insert(section.index(), addr);
        }
        Ok(Self { link, bases })
    }

    pub fn address(&self, file: &object::File<'_>, name: &str) -> Option<u64> {
        let section = file.section_by_name(name)?;
        self.bases.get(&section.index()).copied()
    }

    pub fn load(&self, file: &object::File<'_>, name: &str) -> Result<Vec<u8>, String> {
        let Some(section) = file.section_by_name(name) else {
            return Ok(Vec::new());
        };
        let mut data = section
            .uncompressed_data()
            .map_err(|e| format!("{name}: {e}"))?
            .into_owned();
        // --emit-relocs preserves relocation records in ET_EXEC output,
        // but its contents have already been relocated by the linker.
        if file.kind() != object::ObjectKind::Relocatable {
            return Ok(data);
        }
        for (offset, relocation) in section.relocations() {
            if relocation.kind() == RelocationKind::None {
                continue;
            }
            let error = |why: &str| format!("{name} relocation at {offset:#x}: {why}");
            if relocation.encoding() != RelocationEncoding::Generic
                || relocation.size() != 32
                || !matches!(
                    relocation.kind(),
                    RelocationKind::Absolute | RelocationKind::Relative
                )
            {
                return Err(error(&format!(
                    "unsupported 68k relocation {:?}",
                    relocation.flags()
                )));
            }
            let base = |index: SectionIndex| {
                self.bases
                    .get(&index)
                    .copied()
                    .ok_or_else(|| error("invalid target section"))
            };
            let target = match relocation.target() {
                RelocationTarget::Symbol(index) => {
                    let symbol = file
                        .symbol_by_index(index)
                        .map_err(|e| error(&e.to_string()))?;
                    match symbol.section() {
                        SymbolSection::Section(index) => base(index)? + symbol.address(),
                        SymbolSection::Absolute => symbol.address(),
                        _ => return Err(error("undefined or unsupported target symbol")),
                    }
                }
                RelocationTarget::Section(index) => base(index)?,
                RelocationTarget::Absolute => 0,
                _ => return Err(error("unsupported relocation target")),
            };
            let at = usize::try_from(offset).map_err(|_| error("offset outside section"))?;
            let end = at
                .checked_add(4)
                .ok_or_else(|| error("offset outside section"))?;
            let word = data
                .get_mut(at..end)
                .ok_or_else(|| error("offset outside section"))?;
            // RELA supplies the complete addend; only REL adds the stored
            // word. Wrapping arithmetic matches a 32-bit m68k relocation.
            let mut value = (target as u32).wrapping_add(relocation.addend() as u32);
            if relocation.has_implicit_addend() {
                value = value.wrapping_add(u32::from_be_bytes(word.try_into().unwrap()));
            }
            if relocation.kind() == RelocationKind::Relative {
                value = value.wrapping_sub((base(section.index())? + offset) as u32);
            }
            word.copy_from_slice(&value.to_be_bytes());
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELF: &[u8] = include_bytes!("../../../guest/dap-test/reloc/program-dwarf5.elf");

    // Alter one real ELF32 relocation to cover addend/target forms the
    // compiler fixtures do not happen to emit. The surrounding sections
    // and symbol table remain those of the reproducible GCC fixture.
    struct Fixture {
        bytes: Vec<u8>,
        relocation: usize,
        header: usize,
        word: usize,
        symbol: usize,
        expected: u32,
        offset: u32,
    }

    fn put32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
    }

    impl Fixture {
        fn new() -> Self {
            let file = object::File::parse(ELF).unwrap();
            let sections = Sections::new(&file).unwrap();
            let rela = file.section_by_name(".rela.debug_info").unwrap();
            let relocation = rela.file_range().unwrap().0 as usize;
            let section = file.section_by_name(".debug_info").unwrap();
            let offset = section.relocations().next().unwrap().0 as u32;
            let word = section.file_range().unwrap().0 as usize + offset as usize;
            let worker = file.symbols().find(|s| s.name() == Ok("worker")).unwrap();
            let symbol = file
                .section_by_name(".symtab")
                .unwrap()
                .file_range()
                .unwrap()
                .0 as usize
                + worker.index().0 * 16;
            let shoff = u32::from_be_bytes(ELF[32..36].try_into().unwrap()) as usize;
            let header = shoff + rela.index().0 * 40;
            let expected = sections.bases[&worker.section_index().unwrap()] as u32 + 6;
            let mut bytes = ELF.to_vec();
            put32(&mut bytes, symbol + 4, 8); // st_value, relative to worker's section
            put32(
                &mut bytes,
                relocation + 4,
                ((worker.index().0 as u32) << 8) | object::elf::R_68K_32.0,
            );
            put32(&mut bytes, relocation + 8, (-2i32) as u32);
            put32(&mut bytes, word, 0x1234_5678);
            Self {
                bytes,
                relocation,
                header,
                word,
                symbol,
                expected,
                offset,
            }
        }

        fn load(&self) -> Result<Vec<u8>, String> {
            let file = object::File::parse(self.bytes.as_slice()).unwrap();
            Sections::new(&file)?.load(&file, ".debug_info")
        }

        fn value(&self) -> u32 {
            let data = self.load().unwrap();
            let at = self.offset as usize;
            u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
        }
    }

    #[test]
    fn rela_uses_symbol_value_and_signed_addend_not_stored_word() {
        let fixture = Fixture::new();
        assert_eq!(fixture.value(), fixture.expected);
    }

    #[test]
    fn rel_uses_the_implicit_big_endian_addend() {
        let mut fixture = Fixture::new();
        put32(
            &mut fixture.bytes,
            fixture.header + 4,
            object::elf::SHT_REL.0,
        );
        put32(&mut fixture.bytes, fixture.header + 20, 8); // one Elf32_Rel
        put32(&mut fixture.bytes, fixture.header + 36, 8); // sh_entsize
        put32(&mut fixture.bytes, fixture.word, (-2i32) as u32);
        assert_eq!(fixture.value(), fixture.expected);
    }

    #[test]
    fn pc_relative_relocation_subtracts_place_from_absolute_symbol() {
        let mut fixture = Fixture::new();
        fixture.bytes[fixture.relocation + 7] = object::elf::R_68K_PC32.0 as u8;
        fixture.bytes[fixture.symbol + 14..fixture.symbol + 16]
            .copy_from_slice(&object::elf::SHN_ABS.0.to_be_bytes());
        put32(&mut fixture.bytes, fixture.symbol + 4, 1);
        assert_eq!(fixture.value(), u32::MAX.wrapping_sub(fixture.offset));
    }

    #[test]
    fn malformed_or_unsupported_relocations_report_section_and_offset() {
        let mut fixture = Fixture::new();
        fixture.bytes[fixture.relocation + 7] = object::elf::R_68K_16.0 as u8;
        let error = fixture.load().unwrap_err();
        assert!(error.contains(".debug_info relocation at 0x8"), "{error}");
        assert!(error.contains("unsupported 68k relocation"), "{error}");

        let mut fixture = Fixture::new();
        put32(&mut fixture.bytes, fixture.relocation, u32::MAX);
        assert!(fixture
            .load()
            .unwrap_err()
            .contains("offset outside section"));

        let mut fixture = Fixture::new();
        fixture.bytes[fixture.symbol + 14..fixture.symbol + 16].fill(0);
        assert!(fixture.load().unwrap_err().contains("undefined"));
    }

    #[test]
    fn linked_elf_debug_sections_with_emit_relocs_are_already_resolved() {
        let bytes = include_bytes!("../../../guest/dap-test/reloc/program-linked.elf");
        let file = object::File::parse(bytes.as_slice()).unwrap();
        assert_eq!(file.kind(), object::ObjectKind::Executable);
        let sections = Sections::new(&file).unwrap();
        for name in [".debug_info", ".debug_line", ".debug_frame"] {
            let section = file.section_by_name(name).unwrap();
            assert!(section.relocations().next().is_some(), "{name}");
            assert_eq!(sections.load(&file, name).unwrap(), section.data().unwrap());
        }
    }
}
