// SPDX-License-Identifier: GPL-3.0-or-later

//! AmigaDOS hunk executables, read for their debug payload: the hunk
//! table (kinds and sizes, so addresses can be expressed as hunk +
//! offset), `HUNK_SYMBOL` tables, `HUNK_DEBUG` blocks in the SAS/C
//! `LINE` format that vasm's `-linedebug` writes, and the trailing
//! `HUNK_DEBUG` block bebbo's amiga-gcc `-g` appends after the last hunk,
//! which carries the DWARF sections back to back with a symbol table
//! (`__debug_info_start`, `__debug_line_start`, ...) naming where each
//! begins.
//!
//! Relocation blocks are walked, not applied: the guest loader relocates
//! the program, and the addresses the debugger needs come from the
//! segment list it built (`segments.list`).

use std::fmt;

pub const HUNK_UNIT: u32 = 0x3E7;
pub const HUNK_NAME: u32 = 0x3E8;
pub const HUNK_CODE: u32 = 0x3E9;
pub const HUNK_DATA: u32 = 0x3EA;
pub const HUNK_BSS: u32 = 0x3EB;
pub const HUNK_RELOC32: u32 = 0x3EC;
pub const HUNK_RELOC16: u32 = 0x3ED;
pub const HUNK_RELOC8: u32 = 0x3EE;
pub const HUNK_EXT: u32 = 0x3EF;
pub const HUNK_SYMBOL: u32 = 0x3F0;
pub const HUNK_DEBUG: u32 = 0x3F1;
pub const HUNK_END: u32 = 0x3F2;
pub const HUNK_HEADER: u32 = 0x3F3;
pub const HUNK_OVERLAY: u32 = 0x3F5;
pub const HUNK_BREAK: u32 = 0x3F6;
/// In an executable, `HUNK_DREL32` carries the same 16-bit short
/// relocation format as `HUNK_RELOC32SHORT` (the V39 loader treats
/// them alike), so both are read the same way.
pub const HUNK_DREL32: u32 = 0x3F7;
pub const HUNK_DREL16: u32 = 0x3F8;
pub const HUNK_DREL8: u32 = 0x3F9;
pub const HUNK_LIB: u32 = 0x3FA;
pub const HUNK_INDEX: u32 = 0x3FB;
pub const HUNK_RELOC32SHORT: u32 = 0x3FC;
pub const HUNK_RELRELOC32: u32 = 0x3FD;
pub const HUNK_ABSRELOC16: u32 = 0x3FE;

/// The type word's memory-flag bits (`HUNKF_CHIP`, `HUNKF_FAST`).
const HUNK_TYPE_MASK: u32 = 0x3FFF_FFFF;
/// Hunk-table sizes carry the same two flag bits; both set means an
/// explicit memory-attribute longword follows.
const SIZE_FLAGS_SHIFT: u32 = 30;

/// A hunk's loader kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkKind {
    Code,
    Data,
    Bss,
}

impl HunkKind {
    pub fn name(self) -> &'static str {
        match self {
            HunkKind::Code => "code",
            HunkKind::Data => "data",
            HunkKind::Bss => "bss",
        }
    }
}

/// Where the loader must put a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemKind {
    #[default]
    Any,
    Chip,
    Fast,
    /// Explicit `MEMF_*` attribute longword from the hunk table.
    Attr(u32),
}

/// One `HUNK_SYMBOL` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkSymbol {
    pub name: String,
    /// Offset from the hunk's payload start.
    pub offset: u32,
    /// elf2hunk writes `name@size`; the size is split off here.
    pub size: Option<u32>,
}

/// One `LINE` debug block: source name and `(line, hunk offset)` rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDebug {
    pub file: String,
    pub rows: Vec<(u32, u32)>,
}

/// One hunk of the executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub kind: HunkKind,
    /// Allocation size in bytes from the hunk table (a BSS hunk's whole
    /// size; code/data may be padded beyond their payload).
    pub size: u32,
    pub mem: MemKind,
    /// The payload of a code or data hunk (empty for BSS).
    pub data: Vec<u8>,
    pub symbols: Vec<HunkSymbol>,
    pub lines: Vec<LineDebug>,
    /// `HUNK_DEBUG` blocks in a format other than `LINE`, kept raw.
    pub debug_raw: Vec<Vec<u8>>,
}

/// The DWARF payload of a bebbo-style trailing debug block: the section
/// bytes back to back, and each section's start named by a symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DwarfBlock {
    pub data: Vec<u8>,
    /// `(section name without the leading ".debug_", start offset)`,
    /// sorted by offset.
    pub sections: Vec<(String, u32)>,
}

impl DwarfBlock {
    /// The bytes of `.debug_<name>` (`"info"`, `"line"`, ...), empty
    /// when the block has no such section.
    pub fn section(&self, name: &str) -> &[u8] {
        let Some(pos) = self.sections.iter().position(|(n, _)| n == name) else {
            return &[];
        };
        let start = self.sections[pos].1 as usize;
        let end = self
            .sections
            .get(pos + 1)
            .map_or(self.data.len(), |(_, off)| *off as usize);
        let end = end.min(self.data.len());
        let start = start.min(end);
        &self.data[start..end]
    }

    fn from_trailing(data: Vec<u8>, symbols: &[HunkSymbol]) -> Option<Self> {
        let mut sections: Vec<(String, u32)> = symbols
            .iter()
            .filter_map(|s| {
                let name = s.name.strip_prefix("__debug_")?.strip_suffix("_start")?;
                Some((name.to_string(), s.offset))
            })
            .collect();
        if sections.is_empty() {
            return None;
        }
        sections.sort_by_key(|(_, off)| *off);
        Some(Self { data, sections })
    }
}

/// A parsed executable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HunkFile {
    pub hunks: Vec<Hunk>,
    pub dwarf: Option<DwarfBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkError(pub String);

impl fmt::Display for HunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HunkError {}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Result<u32, HunkError> {
        let end = self.pos.checked_add(4).ok_or_else(|| self.truncated())?;
        let b = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.truncated())?;
        self.pos = end;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u16(&mut self) -> Result<u16, HunkError> {
        let end = self.pos.checked_add(2).ok_or_else(|| self.truncated())?;
        let b = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.truncated())?;
        self.pos = end;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], HunkError> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.truncated())?;
        let b = self
            .bytes
            .get(self.pos..end)
            .ok_or_else(|| self.truncated())?;
        self.pos = end;
        Ok(b)
    }

    fn longwords(&mut self, n: u32) -> Result<&'a [u8], HunkError> {
        let n = usize::try_from(n).map_err(|_| self.truncated())?;
        let bytes = n.checked_mul(4).ok_or_else(|| self.truncated())?;
        self.bytes(bytes)
    }

    fn align4(&mut self) {
        self.pos = (self.pos + 3) & !3;
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn truncated(&self) -> HunkError {
        HunkError(format!("truncated hunk file at offset {}", self.pos))
    }
}

/// A padded, NUL-terminated name of `n` longwords.
fn name_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn read_symbols(r: &mut Reader) -> Result<Vec<HunkSymbol>, HunkError> {
    let mut out = Vec::new();
    loop {
        let n = r.u32()?;
        if n == 0 {
            break;
        }
        let raw = name_of(r.longwords(n)?);
        let offset = r.u32()?;
        // elf2hunk: "name@size" (size in hex).
        let (name, size) = match raw.rsplit_once('@') {
            Some((name, size)) if !name.is_empty() => match u32::from_str_radix(size, 16) {
                Ok(size) => (name.to_string(), Some(size)),
                Err(_) => (raw.clone(), None),
            },
            _ => (raw.clone(), None),
        };
        out.push(HunkSymbol { name, offset, size });
    }
    Ok(out)
}

/// The 32-bit relocation block shape (`count, hunk, offsets...` until
/// a zero count); the offsets are skipped.
fn skip_reloc32(r: &mut Reader) -> Result<(), HunkError> {
    loop {
        let count = r.u32()?;
        if count == 0 {
            break;
        }
        let _target = r.u32()?;
        r.longwords(count)?;
    }
    Ok(())
}

/// The 16-bit short relocation block shape, padded to a longword.
fn skip_reloc_short(r: &mut Reader) -> Result<(), HunkError> {
    loop {
        let count = r.u16()?;
        if count == 0 {
            break;
        }
        let _target = r.u16()?;
        r.bytes(usize::from(count) * 2)?;
    }
    r.align4();
    Ok(())
}

/// Parse a `HUNK_DEBUG` payload as a `LINE` block.
fn parse_line_debug(block: &[u8]) -> Option<LineDebug> {
    if block.len() < 12 || &block[4..8] != b"LINE" {
        return None;
    }
    let mut r = Reader {
        bytes: block,
        pos: 0,
    };
    let base = r.u32().ok()?;
    r.pos = 8;
    let name_longs = r.u32().ok()?;
    let file = name_of(r.longwords(name_longs).ok()?);
    let mut rows = Vec::new();
    while r.bytes.len() - r.pos >= 8 {
        let line = r.u32().ok()?;
        let offset = r.u32().ok()?;
        rows.push((line, base.wrapping_add(offset)));
    }
    Some(LineDebug { file, rows })
}

impl HunkFile {
    /// Parse an executable (`HUNK_HEADER` first). Overlaid executables
    /// and object/library files are rejected.
    pub fn parse(bytes: &[u8]) -> Result<Self, HunkError> {
        let mut r = Reader { bytes, pos: 0 };
        if r.u32()? != HUNK_HEADER {
            return Err(HunkError("not a hunk executable (no HUNK_HEADER)".into()));
        }
        // Resident library names (always empty in a loadable file).
        loop {
            let n = r.u32()?;
            if n == 0 {
                break;
            }
            r.longwords(n)?;
        }
        let _table_size = r.u32()?;
        let first = r.u32()?;
        let last = r.u32()?;
        if last < first || last - first > 4096 {
            return Err(HunkError(format!("implausible hunk table {first}..{last}")));
        }
        let mut table = Vec::with_capacity((last - first + 1) as usize);
        for _ in first..=last {
            let word = r.u32()?;
            let flags = word >> SIZE_FLAGS_SHIFT;
            let size = (word & HUNK_TYPE_MASK).saturating_mul(4);
            let mem = match flags {
                0 => MemKind::Any,
                1 => MemKind::Chip,
                2 => MemKind::Fast,
                _ => MemKind::Attr(r.u32()?),
            };
            table.push((size, mem));
        }

        let mut file = HunkFile::default();
        let mut current: Option<Hunk> = None;
        // Blocks after the last hunk's HUNK_END: bebbo's DWARF carrier.
        let mut trailing_debug: Vec<u8> = Vec::new();
        let mut trailing_symbols: Vec<HunkSymbol> = Vec::new();
        let mut in_trailer = false;

        while !r.at_end() {
            let word = r.u32()?;
            let kind = word & HUNK_TYPE_MASK;
            match kind {
                HUNK_CODE | HUNK_DATA | HUNK_BSS => {
                    if let Some(done) = current.take() {
                        // A hunk without HUNK_END before the next one:
                        // tolerate, the loader does too.
                        file.hunks.push(done);
                    }
                    let n = r.u32()?;
                    let index = file.hunks.len();
                    let (size, mem) = table
                        .get(index)
                        .copied()
                        .unwrap_or_else(|| (n.saturating_mul(4), MemKind::Any));
                    let (hk, data) = match kind {
                        HUNK_CODE => (HunkKind::Code, r.longwords(n)?.to_vec()),
                        HUNK_DATA => (HunkKind::Data, r.longwords(n)?.to_vec()),
                        _ => (HunkKind::Bss, Vec::new()),
                    };
                    let mem = match (mem, word >> SIZE_FLAGS_SHIFT) {
                        (MemKind::Any, 1) => MemKind::Chip,
                        (MemKind::Any, 2) => MemKind::Fast,
                        (mem, _) => mem,
                    };
                    current = Some(Hunk {
                        kind: hk,
                        size: size.max(n.saturating_mul(4)),
                        mem,
                        data,
                        symbols: Vec::new(),
                        lines: Vec::new(),
                        debug_raw: Vec::new(),
                    });
                    // Stray debug blocks before a hunk are not the
                    // DWARF carrier, which follows the last hunk.
                    in_trailer = false;
                    trailing_debug.clear();
                    trailing_symbols.clear();
                }
                HUNK_RELOC32 | HUNK_RELOC16 | HUNK_RELOC8 | HUNK_RELRELOC32 | HUNK_ABSRELOC16 => {
                    skip_reloc32(&mut r)?
                }
                HUNK_RELOC32SHORT | HUNK_DREL32 | HUNK_DREL16 | HUNK_DREL8 => {
                    skip_reloc_short(&mut r)?
                }
                HUNK_SYMBOL => {
                    let symbols = read_symbols(&mut r)?;
                    match current.as_mut() {
                        Some(hunk) => hunk.symbols.extend(symbols),
                        None => {
                            in_trailer = true;
                            trailing_symbols.extend(symbols);
                        }
                    }
                }
                HUNK_DEBUG => {
                    let n = r.u32()?;
                    let block = r.longwords(n)?;
                    match current.as_mut() {
                        Some(hunk) => match parse_line_debug(block) {
                            Some(lines) => hunk.lines.push(lines),
                            None => hunk.debug_raw.push(block.to_vec()),
                        },
                        None => {
                            in_trailer = true;
                            trailing_debug.extend_from_slice(block);
                        }
                    }
                }
                HUNK_END => {
                    if let Some(done) = current.take() {
                        file.hunks.push(done);
                    } else if in_trailer {
                        let data = std::mem::take(&mut trailing_debug);
                        let symbols = std::mem::take(&mut trailing_symbols);
                        if file.dwarf.is_none() {
                            file.dwarf = DwarfBlock::from_trailing(data, &symbols);
                        }
                        in_trailer = false;
                    }
                }
                HUNK_NAME | HUNK_UNIT => {
                    let n = r.u32()?;
                    r.longwords(n)?;
                }
                HUNK_OVERLAY | HUNK_BREAK => {
                    return Err(HunkError("overlaid executables are not supported".into()));
                }
                HUNK_EXT | HUNK_LIB | HUNK_INDEX => {
                    return Err(HunkError(
                        "object or library hunks in an executable are not supported".into(),
                    ));
                }
                other => {
                    return Err(HunkError(format!(
                        "unknown hunk type ${other:X} at offset {}",
                        r.pos - 4
                    )));
                }
            }
        }
        if let Some(done) = current.take() {
            file.hunks.push(done);
        }
        if in_trailer && file.dwarf.is_none() {
            // A carrier without its closing HUNK_END, tolerated like a
            // hunk without one.
            file.dwarf = DwarfBlock::from_trailing(trailing_debug, &trailing_symbols);
        }
        if file.hunks.is_empty() {
            return Err(HunkError("no hunks in executable".into()));
        }
        Ok(file)
    }

    /// The index of the first hunk of `kind`.
    pub fn first_of(&self, kind: HunkKind) -> Option<usize> {
        self.hunks.iter().position(|h| h.kind == kind)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A little hunk-file builder for the tests.
    #[derive(Default)]
    pub(crate) struct Builder {
        body: Vec<u8>,
        sizes: Vec<u32>,
    }

    pub(crate) fn lw(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_be_bytes());
    }

    pub(crate) fn padded_name(out: &mut Vec<u8>, name: &str) -> u32 {
        let longs = (name.len() as u32 + 4) / 4; // room for the NUL
        let mut bytes = name.as_bytes().to_vec();
        bytes.resize(longs as usize * 4, 0);
        out.extend_from_slice(&bytes);
        longs
    }

    impl Builder {
        pub(crate) fn hunk(&mut self, kind: u32, data: &[u8], size_word: Option<u32>) -> &mut Self {
            let longs = (data.len() as u32).div_ceil(4);
            self.sizes.push(size_word.unwrap_or(longs));
            lw(&mut self.body, kind);
            lw(&mut self.body, longs);
            if kind != HUNK_BSS {
                let mut d = data.to_vec();
                d.resize(longs as usize * 4, 0);
                self.body.extend_from_slice(&d);
            }
            self
        }

        pub(crate) fn symbols(&mut self, syms: &[(&str, u32)]) -> &mut Self {
            lw(&mut self.body, HUNK_SYMBOL);
            for (name, off) in syms {
                let mut name_bytes = Vec::new();
                let longs = padded_name(&mut name_bytes, name);
                lw(&mut self.body, longs);
                self.body.extend_from_slice(&name_bytes);
                lw(&mut self.body, *off);
            }
            lw(&mut self.body, 0);
            self
        }

        pub(crate) fn line_debug(
            &mut self,
            base: u32,
            file: &str,
            rows: &[(u32, u32)],
        ) -> &mut Self {
            let mut block = Vec::new();
            lw(&mut block, base);
            block.extend_from_slice(b"LINE");
            let mut name_bytes = Vec::new();
            let longs = padded_name(&mut name_bytes, file);
            lw(&mut block, longs);
            block.extend_from_slice(&name_bytes);
            for (line, off) in rows {
                lw(&mut block, *line);
                lw(&mut block, *off);
            }
            lw(&mut self.body, HUNK_DEBUG);
            lw(&mut self.body, block.len() as u32 / 4);
            self.body.extend_from_slice(&block);
            self
        }

        pub(crate) fn raw_debug(&mut self, block: &[u8]) -> &mut Self {
            let mut b = block.to_vec();
            b.resize(b.len().div_ceil(4) * 4, 0);
            lw(&mut self.body, HUNK_DEBUG);
            lw(&mut self.body, b.len() as u32 / 4);
            self.body.extend_from_slice(&b);
            self
        }

        pub(crate) fn reloc32(&mut self, target: u32, offsets: &[u32]) -> &mut Self {
            lw(&mut self.body, HUNK_RELOC32);
            lw(&mut self.body, offsets.len() as u32);
            lw(&mut self.body, target);
            for o in offsets {
                lw(&mut self.body, *o);
            }
            lw(&mut self.body, 0);
            self
        }

        pub(crate) fn reloc_short(&mut self, kind: u32, target: u16, offsets: &[u16]) -> &mut Self {
            lw(&mut self.body, kind);
            self.body
                .extend_from_slice(&(offsets.len() as u16).to_be_bytes());
            self.body.extend_from_slice(&target.to_be_bytes());
            for o in offsets {
                self.body.extend_from_slice(&o.to_be_bytes());
            }
            self.body.extend_from_slice(&0u16.to_be_bytes());
            while !self.body.len().is_multiple_of(4) {
                self.body.push(0);
            }
            self
        }

        pub(crate) fn end(&mut self) -> &mut Self {
            lw(&mut self.body, HUNK_END);
            self
        }

        pub(crate) fn raw(&mut self, words: &[u32]) -> &mut Self {
            for w in words {
                lw(&mut self.body, *w);
            }
            self
        }

        pub(crate) fn build(&self) -> Vec<u8> {
            let mut out = Vec::new();
            lw(&mut out, HUNK_HEADER);
            lw(&mut out, 0);
            lw(&mut out, self.sizes.len() as u32);
            lw(&mut out, 0);
            lw(&mut out, self.sizes.len() as u32 - 1);
            for s in &self.sizes {
                lw(&mut out, *s);
            }
            out.extend_from_slice(&self.body);
            out
        }
    }

    #[test]
    fn parses_code_data_bss_with_symbols_and_line_debug() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0x70, 0x00, 0x4E, 0x75], None)
            .symbols(&[("start", 0), ("sub", 2)])
            .line_debug(0, "/src/t.s", &[(3, 0), (4, 2)])
            .end()
            .hunk(HUNK_DATA, &[0, 0, 0x04, 0xD2], None)
            .reloc32(0, &[0])
            .end()
            .hunk(HUNK_BSS, &[0; 40], None)
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks.len(), 3);
        assert_eq!(file.hunks[0].kind, HunkKind::Code);
        assert_eq!(file.hunks[0].data, vec![0x70, 0x00, 0x4E, 0x75]);
        assert_eq!(file.hunks[0].symbols[1].name, "sub");
        assert_eq!(file.hunks[0].symbols[1].offset, 2);
        assert_eq!(file.hunks[0].lines[0].file, "/src/t.s");
        assert_eq!(file.hunks[0].lines[0].rows, vec![(3, 0), (4, 2)]);
        assert_eq!(file.hunks[1].kind, HunkKind::Data);
        assert_eq!(file.hunks[2].kind, HunkKind::Bss);
        assert_eq!(file.hunks[2].size, 40);
        assert!(file.dwarf.is_none());
    }

    #[test]
    fn line_debug_adds_the_base_offset() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 16], None)
            .line_debug(8, "t.s", &[(10, 0), (11, 4)])
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks[0].lines[0].rows, vec![(10, 8), (11, 12)]);
    }

    #[test]
    fn short_relocations_are_padded_to_a_longword() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 8], None)
            // Three 16-bit offsets: count + target + 3 offsets + terminator
            // = 12 bytes, already aligned; five offsets = 16 bytes.
            .reloc_short(HUNK_RELOC32SHORT, 1, &[0, 2, 4])
            .reloc_short(HUNK_DREL32, 1, &[0, 2])
            .symbols(&[("after", 4)])
            .end()
            .hunk(HUNK_DATA, &[0; 4], None)
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks[0].symbols[0].name, "after");
        assert_eq!(file.hunks.len(), 2);
    }

    #[test]
    fn memory_flags_come_from_the_size_table_and_type_word() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 4], Some(1 | (1 << 30)))
            .end()
            .hunk(HUNK_DATA | (2 << 30), &[0; 4], None)
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks[0].mem, MemKind::Chip);
        assert_eq!(file.hunks[1].mem, MemKind::Fast);
        assert_eq!(file.hunks[1].kind, HunkKind::Data);
    }

    #[test]
    fn elf2hunk_symbol_sizes_are_split_off() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 4], None)
            .symbols(&[("main@1c", 0), ("plain", 2)])
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks[0].symbols[0].name, "main");
        assert_eq!(file.hunks[0].symbols[0].size, Some(0x1c));
        assert_eq!(file.hunks[0].symbols[1].size, None);
    }

    #[test]
    fn trailing_debug_block_becomes_the_dwarf_carrier() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 4], None).end();
        // After the last hunk: DEBUG (raw DWARF bytes), SYMBOL naming the
        // section starts, END -- what amiga-gcc -g writes.
        let dwarf: Vec<u8> = (0u8..32).collect();
        b.raw_debug(&dwarf)
            .symbols(&[
                ("__debug_line_start", 16),
                ("__debug_info_start", 0),
                ("__debug_abbrev_start", 8),
            ])
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert_eq!(file.hunks.len(), 1);
        let block = file.dwarf.expect("dwarf block");
        assert_eq!(block.section("info"), &dwarf[0..8]);
        assert_eq!(block.section("abbrev"), &dwarf[8..16]);
        assert_eq!(block.section("line"), &dwarf[16..32]);
        assert!(block.section("str").is_empty());
    }

    #[test]
    fn non_line_debug_blocks_inside_a_hunk_are_kept_raw() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 4], None)
            .raw_debug(b"ZMAGICxx")
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        assert!(file.hunks[0].lines.is_empty());
        assert_eq!(file.hunks[0].debug_raw.len(), 1);
    }

    #[test]
    fn rejects_non_executables_and_truncation() {
        assert!(HunkFile::parse(&[0, 0, 3, 0xE7, 0, 0, 0, 0]).is_err());
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 8], None).end();
        let bytes = b.build();
        assert!(HunkFile::parse(&bytes[..bytes.len() - 6]).is_err());
        let mut overlay = Builder::default();
        overlay
            .hunk(HUNK_CODE, &[0; 4], None)
            .end()
            .raw(&[HUNK_OVERLAY]);
        assert!(HunkFile::parse(&overlay.build()).is_err());
    }
}
