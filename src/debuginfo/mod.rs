// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest program debug information for the DAP adapter: symbol tables,
//! source line tables, functions, variables and unwind tables read from
//! the host-side executable (and an optional ELF sibling), and the
//! relocation that maps them onto the addresses the guest loaded the
//! program at.
//!
//! Every address in here is a [`HunkAddr`] (hunk index plus offset)
//! until [`DebugInfo::relocate`] learns the hunk bases from the guest's
//! segment list; the queries then take and return runtime addresses.
//! Three sources feed the same tables:
//!
//! - `HUNK_SYMBOL` tables (every toolchain) -> [`Symbol`]s;
//! - `HUNK_DEBUG` `LINE` blocks (vasm `-linedebug`) -> [`LineRow`]s;
//! - DWARF, from the trailing debug hunk amiga-gcc `-g` writes or from
//!   an ELF the program was converted from (elf2hunk) -> line rows,
//!   [`Function`]s, [`Variable`]s, types and call-frame information.

pub mod dwarf;
pub mod hunk;
pub mod unwind;

use hunk::{HunkFile, HunkKind};

/// An address inside the program before loading: hunk index + offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HunkAddr {
    pub hunk: u32,
    pub offset: u32,
}

impl HunkAddr {
    pub fn new(hunk: u32, offset: u32) -> Self {
        Self { hunk, offset }
    }
}

/// A hunk's kind and allocation size, from the executable's hunk table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunkMeta {
    pub kind: HunkKind,
    pub size: u32,
}

/// A named address: a `HUNK_SYMBOL` entry or an ELF symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub at: HunkAddr,
    pub size: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    /// Normalised with `/` separators, as the producer recorded it.
    pub path: String,
}

/// One line-table row: from `at` on, the code belongs to `line` of
/// `file` until the next row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRow {
    pub at: HunkAddr,
    pub file: u32,
    pub line: u32,
    pub column: u32,
    /// A recommended breakpoint location (DWARF `is_stmt`; always true
    /// for LINE rows).
    pub is_stmt: bool,
    /// The end of a sequence: addresses from here on have no line.
    pub end_sequence: bool,
}

pub type TypeId = usize;

/// Where a variable lives, in the single-operation DWARF location
/// forms the adapter evaluates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Static(HunkAddr),
    /// Frame base + offset (`DW_OP_fbreg`).
    FrameOffset(i64),
    /// Register (DWARF numbering: 0-7 = D0-D7, 8-15 = A0-A7) + offset.
    RegOffset {
        reg: u16,
        offset: i64,
    },
    Register(u16),
    /// The canonical frame address (`DW_OP_call_frame_cfa`).
    CallFrameCfa,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub ty: Option<TypeId>,
    pub location: Location,
    /// The lexical block the variable is visible in, when narrower than
    /// its function: start and byte length.
    pub scope: Option<(HunkAddr, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub at: HunkAddr,
    pub size: u32,
    pub frame_base: Location,
    pub params: Vec<Variable>,
    pub locals: Vec<Variable>,
    pub file: Option<u32>,
    pub line: Option<u32>,
}

impl Function {
    /// Whether `at` (unrelocated) lies inside the function.
    pub fn contains(&self, at: HunkAddr) -> bool {
        at.hunk == self.at.hunk
            && at.offset >= self.at.offset
            && at.offset - self.at.offset < self.size.max(1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Signed,
    Unsigned,
    Float,
    Bool,
    SignedChar,
    UnsignedChar,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub ty: Option<TypeId>,
    pub offset: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeDesc {
    Base {
        name: String,
        size: u32,
        encoding: Encoding,
    },
    Pointer {
        target: Option<TypeId>,
        size: u32,
    },
    Array {
        element: Option<TypeId>,
        count: Option<u64>,
    },
    Struct {
        name: Option<String>,
        size: u32,
        members: Vec<Member>,
        is_union: bool,
    },
    Enum {
        name: Option<String>,
        size: u32,
        values: Vec<(String, i64)>,
    },
    Typedef {
        name: String,
        target: Option<TypeId>,
    },
    Qualified {
        qualifier: &'static str,
        target: Option<TypeId>,
    },
    Function,
    Void,
    Unknown,
}

/// One hunk's place in the link-time address space the DWARF uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkSegment {
    pub addr: u64,
    pub size: u64,
    pub hunk: u32,
}

/// The link-time address space: amiga-gcc's DWARF addresses every hunk
/// from 0 (its sections have VMA 0), an ELF places them by its linker
/// script. Code lookups (line rows, functions, CFI) go through here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkMap {
    pub segments: Vec<LinkSegment>,
}

impl LinkMap {
    /// The hunk address of a link-time address. An address exactly at
    /// a segment's end (a function's `high_pc`) resolves to that segment
    /// when no other contains it.
    pub fn to_hunk(&self, addr: u64) -> Option<HunkAddr> {
        let inside = self
            .segments
            .iter()
            .find(|s| addr >= s.addr && addr - s.addr < s.size);
        let seg = inside.or_else(|| self.segments.iter().find(|s| addr == s.addr + s.size))?;
        u32::try_from(addr - seg.addr)
            .ok()
            .map(|offset| HunkAddr::new(seg.hunk, offset))
    }

    pub fn to_link(&self, at: HunkAddr) -> Option<u64> {
        self.segments
            .iter()
            .find(|s| s.hunk == at.hunk)
            .map(|s| s.addr + u64::from(at.offset))
    }
}

/// A line-table lookup hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineHit {
    pub file: u32,
    pub line: u32,
    pub column: u32,
    /// Index into [`DebugInfo::rows`].
    pub row: usize,
}

/// Everything known about one program.
#[derive(Clone, Debug, Default)]
pub struct DebugInfo {
    pub hunks: Vec<HunkMeta>,
    pub symbols: Vec<Symbol>,
    pub files: Vec<SourceFile>,
    /// Sorted by address.
    pub rows: Vec<LineRow>,
    /// Sorted by address.
    pub functions: Vec<Function>,
    pub globals: Vec<Variable>,
    pub types: Vec<TypeDesc>,
    pub cfi: Option<dwarf::Cfi>,
    pub link: LinkMap,
    /// What was read from where, for the adapter's console.
    pub notes: Vec<String>,
    bases: Vec<u32>,
}

/// Type chains (typedef of pointer to array of ...) followed before a
/// cycle in corrupt DWARF is assumed.
const MAX_TYPE_DEPTH: usize = 32;

pub fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

impl DebugInfo {
    /// Read a hunk executable: its hunk table, symbols, LINE rows, and
    /// the DWARF in a trailing debug hunk when there is one.
    pub fn from_hunk_file(file: &HunkFile) -> Self {
        let mut info = DebugInfo {
            hunks: file
                .hunks
                .iter()
                .map(|h| HunkMeta {
                    kind: h.kind,
                    size: h.size,
                })
                .collect(),
            ..Default::default()
        };
        let mut symbol_count = 0usize;
        let mut line_count = 0usize;
        for (index, hunk) in file.hunks.iter().enumerate() {
            let hunk_index = index as u32;
            for sym in &hunk.symbols {
                info.symbols.push(Symbol {
                    name: sym.name.clone(),
                    at: HunkAddr::new(hunk_index, sym.offset),
                    size: sym.size,
                });
                symbol_count += 1;
            }
            for block in &hunk.lines {
                let file_id = info.intern_file(&block.file);
                for &(line, offset) in &block.rows {
                    info.rows.push(LineRow {
                        at: HunkAddr::new(hunk_index, offset),
                        file: file_id,
                        line,
                        column: 0,
                        is_stmt: true,
                        end_sequence: false,
                    });
                    line_count += 1;
                }
            }
        }
        if symbol_count > 0 {
            info.notes.push(format!("{symbol_count} hunk symbols"));
        }
        if line_count > 0 {
            info.notes.push(format!(
                "{line_count} LINE debug rows in {} file(s)",
                info.files.len()
            ));
        }
        // amiga-gcc links the hunks back to back from address 0 (text,
        // then data, then bss, in hunk order); its DWARF addresses and
        // the debug block's own cross-section offsets are all in that
        // space.
        let mut addr = 0u64;
        for (index, hunk) in file.hunks.iter().enumerate() {
            info.link.segments.push(LinkSegment {
                addr,
                size: u64::from(hunk.size),
                hunk: index as u32,
            });
            addr += u64::from(hunk.size);
        }
        if let Some(block) = &file.dwarf {
            match dwarf::load_block(block, &info.link, &info.symbols) {
                Ok(loaded) => info.merge(loaded, "DWARF from the executable's debug hunk"),
                Err(e) => info.notes.push(format!("debug hunk DWARF not read: {e}")),
            }
        }
        info.finish();
        info
    }

    /// Read a program and, optionally, the ELF it was converted from.
    pub fn load(program: &[u8], elf: Option<&[u8]>) -> Result<Self, String> {
        let file = HunkFile::parse(program).map_err(|e| e.to_string())?;
        let mut info = Self::from_hunk_file(&file);
        if let Some(elf) = elf {
            info.add_elf(elf)?;
        }
        Ok(info)
    }

    /// Add the DWARF of the ELF `elf2hunk` converted into this program.
    pub fn add_elf(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (loaded, link) = dwarf::load_elf(bytes, &self.hunks)?;
        self.link = link;
        self.merge(loaded, "DWARF from the ELF");
        self.finish();
        Ok(())
    }

    fn intern_file(&mut self, path: &str) -> u32 {
        let path = normalize_path(path);
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            return i as u32;
        }
        self.files.push(SourceFile { path });
        (self.files.len() - 1) as u32
    }

    /// Fold a DWARF load into the tables, remapping its file and type
    /// ids onto ours.
    fn merge(&mut self, loaded: dwarf::Loaded, what: &str) {
        let file_map: Vec<u32> = loaded
            .files
            .iter()
            .map(|path| self.intern_file(path))
            .collect();
        let type_base = self.types.len();
        let remap_type = |t: Option<TypeId>| t.map(|t| t + type_base);
        let remap_var = |v: Variable| Variable {
            ty: remap_type(v.ty),
            ..v
        };
        for ty in loaded.types {
            self.types.push(match ty {
                TypeDesc::Pointer { target, size } => TypeDesc::Pointer {
                    target: remap_type(target),
                    size,
                },
                TypeDesc::Array { element, count } => TypeDesc::Array {
                    element: remap_type(element),
                    count,
                },
                TypeDesc::Struct {
                    name,
                    size,
                    members,
                    is_union,
                } => TypeDesc::Struct {
                    name,
                    size,
                    members: members
                        .into_iter()
                        .map(|m| Member {
                            ty: remap_type(m.ty),
                            ..m
                        })
                        .collect(),
                    is_union,
                },
                TypeDesc::Typedef { name, target } => TypeDesc::Typedef {
                    name,
                    target: remap_type(target),
                },
                TypeDesc::Qualified { qualifier, target } => TypeDesc::Qualified {
                    qualifier,
                    target: remap_type(target),
                },
                other => other,
            });
        }
        for row in loaded.rows {
            self.rows.push(LineRow {
                file: file_map.get(row.file as usize).copied().unwrap_or(0),
                ..row
            });
        }
        for f in loaded.functions {
            self.functions.push(Function {
                file: f.file.and_then(|i| file_map.get(i as usize).copied()),
                params: f.params.into_iter().map(remap_var).collect(),
                locals: f.locals.into_iter().map(remap_var).collect(),
                ..f
            });
        }
        self.globals
            .extend(loaded.globals.into_iter().map(remap_var));
        if loaded.cfi.is_some() {
            self.cfi = loaded.cfi;
        }
        self.notes.push(format!(
            "{what}: {} function(s), {} line row(s), {} global(s){}",
            self.functions.len(),
            self.rows.len(),
            self.globals.len(),
            if self.cfi.is_some() {
                ", call-frame info"
            } else {
                ""
            }
        ));
        self.notes.extend(loaded.notes);
    }

    fn finish(&mut self) {
        self.rows.sort_by_key(|r| (r.at, r.end_sequence));
        self.functions.sort_by_key(|f| f.at);
        self.symbols.sort_by_key(|s| s.at);
    }

    // -----------------------------------------------------------------
    // Relocation

    /// Learn where the guest loaded each hunk (first hunk first).
    pub fn relocate(&mut self, bases: Vec<u32>) {
        self.bases = bases;
    }

    pub fn relocated(&self) -> bool {
        !self.bases.is_empty()
    }

    pub fn bases(&self) -> &[u32] {
        &self.bases
    }

    /// The runtime address of `at`, once relocated.
    pub fn runtime(&self, at: HunkAddr) -> Option<u32> {
        self.bases
            .get(at.hunk as usize)
            .map(|base| base.wrapping_add(at.offset))
    }

    /// Which hunk a runtime address falls in.
    pub fn locate(&self, addr: u32) -> Option<HunkAddr> {
        self.bases.iter().enumerate().find_map(|(i, &base)| {
            let size = self.hunks.get(i).map_or(0, |h| h.size);
            (addr >= base && addr - base < size.max(1))
                .then(|| HunkAddr::new(i as u32, addr - base))
        })
    }

    // -----------------------------------------------------------------
    // Queries (runtime addresses)

    /// The line-table row covering `addr`.
    pub fn line_for(&self, addr: u32) -> Option<LineHit> {
        let at = self.locate(addr)?;
        // Last row at or before `at` in the same hunk.
        let idx = self.rows.partition_point(|r| r.at <= at);
        if idx == 0 {
            return None;
        }
        let row = &self.rows[idx - 1];
        if row.at.hunk != at.hunk || row.end_sequence {
            return None;
        }
        Some(LineHit {
            file: row.file,
            line: row.line,
            column: row.column,
            row: idx - 1,
        })
    }

    /// The runtime extent `[start, end)` of the line-row run covering
    /// `addr`: the row's own start to the start of the next row with a
    /// different line (or the sequence end).
    pub fn line_extent(&self, addr: u32) -> Option<(u32, u32)> {
        let hit = self.line_for(addr)?;
        let row = self.rows[hit.row];
        let mut start = hit.row;
        while start > 0 {
            let prev = self.rows[start - 1];
            if prev.at.hunk != row.at.hunk
                || prev.end_sequence
                || prev.file != row.file
                || prev.line != row.line
            {
                break;
            }
            start -= 1;
        }
        let mut end = hit.row + 1;
        while end < self.rows.len() {
            let next = self.rows[end];
            if next.at.hunk != row.at.hunk
                || next.end_sequence
                || next.file != row.file
                || next.line != row.line
            {
                break;
            }
            end += 1;
        }
        let start_addr = self.runtime(self.rows[start].at)?;
        let end_addr = match self.rows.get(end) {
            Some(next) if next.at.hunk == row.at.hunk => self.runtime(next.at)?,
            _ => self
                .bases
                .get(row.at.hunk as usize)
                .map(|b| b.wrapping_add(self.hunks[row.at.hunk as usize].size))?,
        };
        Some((start_addr, end_addr))
    }

    /// The breakpoint addresses for `line` of `file`, or of the nearest
    /// following line that has code when `line` has none (GDB's rule).
    /// Returns the line actually used.
    pub fn resolve_line(&self, file: u32, line: u32) -> Option<(u32, Vec<u32>)> {
        let candidates = self
            .rows
            .iter()
            .filter(|r| r.file == file && !r.end_sequence && r.is_stmt && r.line >= line);
        let target = candidates.map(|r| r.line).min()?;
        let mut addrs = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            if row.file != file || row.end_sequence || !row.is_stmt || row.line != target {
                continue;
            }
            // Only the first row of a run of rows for this line: the
            // rest are the same statement continued.
            let starts_run = i == 0 || {
                let prev = self.rows[i - 1];
                prev.at.hunk != row.at.hunk
                    || prev.end_sequence
                    || prev.file != row.file
                    || prev.line != row.line
            };
            if !starts_run {
                continue;
            }
            if let Some(addr) = self.runtime(row.at) {
                if !addrs.contains(&addr) {
                    addrs.push(addr);
                }
            }
        }
        Some((target, addrs))
    }

    pub fn function_at(&self, addr: u32) -> Option<&Function> {
        let at = self.locate(addr)?;
        let idx = self.functions.partition_point(|f| f.at <= at);
        (0..idx)
            .rev()
            .map(|i| &self.functions[i])
            .take_while(|f| f.at.hunk == at.hunk)
            .find(|f| f.contains(at))
    }

    /// The nearest symbol at or below `addr` in the same hunk, with the
    /// distance to it.
    pub fn symbol_at(&self, addr: u32) -> Option<(&Symbol, u32)> {
        let at = self.locate(addr)?;
        let idx = self.symbols.partition_point(|s| s.at <= at);
        if idx == 0 {
            return None;
        }
        // Several symbols can share an address; prefer the one that looks
        // like a code label (no leading double underscore section marks).
        let best = (0..idx)
            .rev()
            .map(|i| &self.symbols[i])
            .take_while(|s| s.at.hunk == at.hunk)
            .next()?;
        let same_addr = self.symbols[..idx]
            .iter()
            .rev()
            .take_while(|s| s.at == best.at)
            .min_by_key(|s| (s.name.starts_with("__"), s.name.len()))
            .unwrap_or(best);
        Some((same_addr, at.offset - same_addr.at.offset))
    }

    /// A function or symbol by name (`main`, `_main`, `start`), as a
    /// runtime address.
    pub fn lookup(&self, name: &str) -> Option<u32> {
        let alt = match name.strip_prefix('_') {
            Some(rest) => rest.to_string(),
            None => format!("_{name}"),
        };
        if let Some(f) = self
            .functions
            .iter()
            .find(|f| f.name == name || f.name == alt)
        {
            return self.runtime(f.at);
        }
        let sym = self
            .symbols
            .iter()
            .find(|s| s.name == name)
            .or_else(|| self.symbols.iter().find(|s| s.name == alt))?;
        self.runtime(sym.at)
    }

    /// The variable named `name` visible at `pc`: a local or parameter
    /// of the enclosing function first, then a global.
    pub fn variable_at(&self, name: &str, pc: u32) -> Option<&Variable> {
        if let Some(f) = self.function_at(pc) {
            let at = self.locate(pc)?;
            if let Some(v) = f
                .locals
                .iter()
                .chain(f.params.iter())
                .filter(|v| v.name == name)
                .find(|v| match v.scope {
                    None => true,
                    Some((start, len)) => {
                        at.hunk == start.hunk
                            && at.offset >= start.offset
                            && at.offset - start.offset < len.max(1)
                    }
                })
            {
                return Some(v);
            }
        }
        self.globals.iter().find(|v| v.name == name)
    }

    /// The file id whose recorded path best matches `path`: exact, then
    /// the longest case-insensitive path suffix with the same basename.
    pub fn find_file(&self, path: &str) -> Option<u32> {
        let path = normalize_path(path);
        if let Some(i) = self.files.iter().position(|f| f.path == path) {
            return Some(i as u32);
        }
        let want: Vec<String> = path.rsplit('/').map(|c| c.to_ascii_lowercase()).collect();
        let mut best: Option<(usize, usize)> = None; // (matched components, file)
        for (i, f) in self.files.iter().enumerate() {
            let have: Vec<String> = f.path.rsplit('/').map(|c| c.to_ascii_lowercase()).collect();
            let matched = want
                .iter()
                .zip(have.iter())
                .take_while(|(a, b)| a == b)
                .count();
            if matched == 0 {
                continue;
            }
            if best.is_none_or(|(m, _)| matched > m) {
                best = Some((matched, i));
            }
        }
        best.map(|(_, i)| i as u32)
    }

    /// Every statement start inside `f`, as `(runtime address, line)`.
    pub fn function_line_starts(&self, f: &Function) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let mut last: Option<(u32, u32)> = None;
        for row in &self.rows {
            if row.end_sequence || !row.is_stmt || !f.contains(row.at) {
                continue;
            }
            if last == Some((row.file, row.line)) {
                continue;
            }
            last = Some((row.file, row.line));
            if let Some(addr) = self.runtime(row.at) {
                out.push((addr, row.line));
            }
        }
        out
    }

    // -----------------------------------------------------------------
    // Types

    /// Strip typedefs and qualifiers.
    pub fn resolve_type(&self, mut ty: Option<TypeId>) -> Option<TypeId> {
        for _ in 0..32 {
            match self.types.get(ty?)? {
                TypeDesc::Typedef { target, .. } | TypeDesc::Qualified { target, .. } => {
                    ty = *target;
                }
                _ => return ty,
            }
        }
        None
    }

    pub fn type_size(&self, ty: Option<TypeId>) -> Option<u32> {
        self.type_size_depth(ty, 0)
    }

    fn type_size_depth(&self, ty: Option<TypeId>, depth: usize) -> Option<u32> {
        if depth > MAX_TYPE_DEPTH {
            return None;
        }
        match self.types.get(self.resolve_type(ty)?)? {
            TypeDesc::Base { size, .. }
            | TypeDesc::Pointer { size, .. }
            | TypeDesc::Struct { size, .. }
            | TypeDesc::Enum { size, .. } => Some(*size),
            TypeDesc::Array { element, count } => self
                .type_size_depth(*element, depth + 1)?
                .checked_mul(u32::try_from((*count)?).ok()?),
            _ => None,
        }
    }

    pub fn type_name(&self, ty: Option<TypeId>) -> String {
        self.type_name_depth(ty, 0)
    }

    fn type_name_depth(&self, ty: Option<TypeId>, depth: usize) -> String {
        let Some(id) = ty else {
            return "void".to_string();
        };
        if depth > MAX_TYPE_DEPTH {
            return "...".to_string();
        }
        let inner = |t: Option<TypeId>| self.type_name_depth(t, depth + 1);
        match self.types.get(id) {
            Some(TypeDesc::Base { name, .. }) => name.clone(),
            Some(TypeDesc::Pointer { target, .. }) => format!("{} *", inner(*target)),
            Some(TypeDesc::Array { element, count }) => match count {
                Some(n) => format!("{}[{n}]", inner(*element)),
                None => format!("{}[]", inner(*element)),
            },
            Some(TypeDesc::Struct { name, is_union, .. }) => format!(
                "{} {}",
                if *is_union { "union" } else { "struct" },
                name.as_deref().unwrap_or("<anonymous>")
            ),
            Some(TypeDesc::Enum { name, .. }) => {
                format!("enum {}", name.as_deref().unwrap_or("<anonymous>"))
            }
            Some(TypeDesc::Typedef { name, .. }) => name.clone(),
            Some(TypeDesc::Qualified { qualifier, target }) => {
                format!("{qualifier} {}", inner(*target))
            }
            Some(TypeDesc::Function) => "function".to_string(),
            Some(TypeDesc::Void) => "void".to_string(),
            Some(TypeDesc::Unknown) | None => "?".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::hunk::tests::Builder;
    use super::hunk::{HUNK_CODE, HUNK_DATA};
    use super::*;

    fn asm_program() -> DebugInfo {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 16], None)
            .symbols(&[("start", 0), ("sub", 8)])
            .line_debug(
                0,
                "/build/src/main.s",
                &[(3, 0), (4, 2), (5, 4), (9, 8), (10, 12)],
            )
            .end()
            .hunk(HUNK_DATA, &[0; 8], None)
            .symbols(&[("value", 4)])
            .end();
        let file = HunkFile::parse(&b.build()).unwrap();
        let mut info = DebugInfo::from_hunk_file(&file);
        info.relocate(vec![0x1000, 0x2000]);
        info
    }

    #[test]
    fn relocation_maps_both_ways() {
        let info = asm_program();
        assert_eq!(info.runtime(HunkAddr::new(0, 8)), Some(0x1008));
        assert_eq!(info.runtime(HunkAddr::new(1, 4)), Some(0x2004));
        assert_eq!(info.locate(0x100C), Some(HunkAddr::new(0, 12)));
        assert_eq!(info.locate(0x2000), Some(HunkAddr::new(1, 0)));
        assert_eq!(info.locate(0x3000), None);
        assert_eq!(info.locate(0x1010), None, "past the hunk");
    }

    #[test]
    fn line_lookups_follow_the_rows() {
        let info = asm_program();
        let hit = info.line_for(0x1002).unwrap();
        assert_eq!((hit.line, hit.file), (4, 0));
        assert_eq!(info.line_for(0x1003).unwrap().line, 4, "inside the row");
        assert_eq!(info.line_for(0x100E).unwrap().line, 10, "last row extends");
        assert_eq!(info.line_extent(0x1005), Some((0x1004, 0x1008)));
        assert_eq!(info.line_extent(0x100D), Some((0x100C, 0x1010)));
        assert_eq!(info.resolve_line(0, 4), Some((4, vec![0x1002])));
        assert_eq!(
            info.resolve_line(0, 7),
            Some((9, vec![0x1008])),
            "next line with code"
        );
        assert_eq!(info.resolve_line(0, 11), None);
    }

    #[test]
    fn symbols_and_files_resolve() {
        let info = asm_program();
        let (sym, off) = info.symbol_at(0x100A).unwrap();
        assert_eq!((sym.name.as_str(), off), ("sub", 2));
        assert_eq!(info.lookup("sub"), Some(0x1008));
        assert_eq!(info.lookup("_start"), Some(0x1000), "underscore tolerant");
        assert_eq!(info.lookup("value"), Some(0x2004));
        assert_eq!(info.find_file("/build/src/main.s"), Some(0));
        assert_eq!(
            info.find_file("C:\\work\\SRC\\Main.s"),
            Some(0),
            "suffix match"
        );
        assert_eq!(info.find_file("other.s"), None);
    }

    #[test]
    fn unrelocated_info_answers_nothing() {
        let mut b = Builder::default();
        b.hunk(HUNK_CODE, &[0; 4], None)
            .symbols(&[("start", 0)])
            .end();
        let info = DebugInfo::from_hunk_file(&HunkFile::parse(&b.build()).unwrap());
        assert!(!info.relocated());
        assert_eq!(info.lookup("start"), None);
        assert_eq!(info.line_for(0), None);
    }

    #[test]
    fn link_map_resolves_segment_ends() {
        let map = LinkMap {
            segments: vec![
                LinkSegment {
                    addr: 0x100,
                    size: 0x20,
                    hunk: 0,
                },
                LinkSegment {
                    addr: 0x120,
                    size: 0x10,
                    hunk: 1,
                },
            ],
        };
        assert_eq!(map.to_hunk(0x11F), Some(HunkAddr::new(0, 0x1F)));
        assert_eq!(map.to_hunk(0x120), Some(HunkAddr::new(1, 0)), "start wins");
        assert_eq!(
            map.to_hunk(0x130),
            Some(HunkAddr::new(1, 0x10)),
            "end address"
        );
        assert_eq!(map.to_link(HunkAddr::new(1, 4)), Some(0x124));
    }
}

/// Tests over the committed `guest/dap-test` probes: `hello` (amiga-gcc
/// 6.5 `-g`, DWARF in a trailing debug hunk) and `lines` (vasm
/// `-linedebug`, LINE hunks).
#[cfg(test)]
mod fixture_tests {
    use super::unwind::{self, Registers};
    use super::*;

    const HELLO: &[u8] = include_bytes!("../../guest/dap-test/hello");
    const LINES: &[u8] = include_bytes!("../../guest/dap-test/lines");

    fn hello() -> DebugInfo {
        let mut info = DebugInfo::load(HELLO, None).expect("hello parses");
        info.relocate(vec![0x2_0000, 0x3_0000, 0x4_0000]);
        info
    }

    #[test]
    fn hello_has_symbols_functions_and_lines() {
        let info = hello();
        assert_eq!(info.hunks.len(), 3);
        assert!(
            info.notes.iter().any(|n| n.contains("debug hunk")),
            "{:?}",
            info.notes
        );
        let names: Vec<&str> = info.functions.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["add", "scale", "entry"], "{:?}", info.notes);
        let add = &info.functions[0];
        let add_sym = info
            .symbols
            .iter()
            .find(|s| s.name == "_add")
            .expect("_add symbol");
        assert_eq!(add.at, add_sym.at, "DWARF and hunk symbol agree");
        assert_eq!(add.at.hunk, 0);
        assert_eq!(info.lookup("add"), info.runtime(add_sym.at));
        let entry_sym = info
            .symbols
            .iter()
            .find(|s| s.name == "_entry")
            .expect("_entry symbol");
        assert_eq!(
            info.lookup("entry"),
            info.runtime(entry_sym.at),
            "underscore tolerant"
        );
        let entry_addr = info.runtime(entry_sym.at).unwrap();
        assert_eq!(add.params.len(), 2);
        assert_eq!(add.params[0].name, "a");
        assert!(
            matches!(add.params[0].location, Location::FrameOffset(_)),
            "{:?}",
            add.params[0]
        );
        assert_eq!(add.locals[0].name, "sum");
        assert_eq!(info.files.len(), 1);
        assert!(
            info.files[0].path.ends_with("hello.c"),
            "{}",
            info.files[0].path
        );
        // The entry point's first statement is a line of hello.c.
        let hit = info.line_for(entry_addr).expect("line at entry");
        assert!(hit.line > 40, "line {}", hit.line);
        let function_line_starts = info.function_line_starts(&info.functions[2]);
        assert!(function_line_starts.len() >= 8, "{function_line_starts:?}");
        // A source breakpoint on the function's line resolves to its
        // first statement address.
        let file = info
            .find_file("/somewhere/else/hello.c")
            .expect("suffix match");
        let (line, addrs) = info.resolve_line(file, hit.line).unwrap();
        assert_eq!(line, hit.line);
        assert!(addrs.contains(&entry_addr), "{addrs:?}");
    }

    #[test]
    fn hello_globals_and_types() {
        let info = hello();
        let counter = info
            .globals
            .iter()
            .find(|g| g.name == "counter")
            .expect("counter");
        assert_eq!(
            counter.location,
            Location::Static(HunkAddr::new(1, 0)),
            "resolved by symbol"
        );
        assert_eq!(info.type_size(counter.ty), Some(4));
        assert_eq!(info.type_name(counter.ty), "LONG");
        let origin = info
            .globals
            .iter()
            .find(|g| g.name == "origin")
            .expect("origin");
        assert_eq!(origin.location, Location::Static(HunkAddr::new(1, 8)));
        let resolved = info.resolve_type(origin.ty).unwrap();
        match &info.types[resolved] {
            TypeDesc::Struct {
                name,
                members,
                size,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("point"));
                assert_eq!(*size, 8);
                let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
                assert_eq!(names, vec!["x", "y"]);
                assert_eq!(members[1].offset, 4);
            }
            other => panic!("origin is {other:?}"),
        }
        let greeting = info
            .globals
            .iter()
            .find(|g| g.name == "greeting")
            .expect("greeting");
        assert!(matches!(
            info.resolve_type(greeting.ty).map(|t| &info.types[t]),
            Some(TypeDesc::Pointer { .. })
        ));
        let flag = info
            .globals
            .iter()
            .find(|g| g.name == "flag")
            .expect("flag");
        assert_eq!(
            flag.location,
            Location::Static(HunkAddr::new(2, 4)),
            "bss, by symbol"
        );
    }

    #[test]
    fn hello_cfi_unwinds_a_synthetic_call() {
        let info = hello();
        let add = info
            .functions
            .iter()
            .find(|f| f.name == "add")
            .unwrap()
            .clone();
        let scale = info
            .functions
            .iter()
            .find(|f| f.name == "scale")
            .unwrap()
            .clone();
        let cfi = info.cfi.as_ref().expect("call-frame info");
        let row = cfi.row_for(u64::from(add.at.offset)).expect("row at add");
        assert_eq!((row.cfa_reg, row.cfa_offset), (15, 4));
        assert_eq!(row.ra_reg, 24);
        assert!(
            row.rules.contains(&(24, dwarf::RegRule::Offset(-4))),
            "{:?}",
            row.rules
        );
        // At add's first instruction with the return address on the
        // stack: the caller frame is a call site inside scale.
        let add_addr = info.runtime(add.at).unwrap();
        let call_site = info.runtime(scale.at).unwrap() + 8;
        let regs = Registers {
            a: [0, 0, 0, 0, 0, 0, 0, 0x8000],
            pc: add_addr,
            ..Default::default()
        };
        let stack = move |addr: u32| match addr {
            0x8000 => Some(call_site),
            0x8004 => Some(0x0000_0001),
            0x8008 => Some(0x0000_0000),
            _ => Some(0),
        };
        let mut read32 = stack;
        let mut read16 = |addr: u32| {
            // Code words for the return-address scan: hello's own code.
            let at = info.locate(addr)?;
            let hunk = hunk::HunkFile::parse(HELLO).ok()?;
            let data = &hunk.hunks[at.hunk as usize].data;
            let off = at.offset as usize;
            data.get(off..off + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        };
        let frames = unwind::unwind(&info, &regs, &mut read32, &mut read16, 8);
        assert!(frames.len() >= 2, "{frames:?}");
        assert_eq!(frames[1].pc, call_site);
        assert_eq!(frames[1].sp, 0x8004);
        assert_eq!(frames[1].via, unwind::FrameVia::Cfi);
        assert_eq!(
            info.function_at(frames[1].pc - 2).map(|f| f.name.as_str()),
            Some("scale")
        );
    }

    #[test]
    fn lines_probe_has_line_rows_and_symbols() {
        let mut info = DebugInfo::load(LINES, None).expect("lines parses");
        assert_eq!(info.hunks.len(), 2);
        assert!(
            info.notes.iter().any(|n| n.contains("LINE")),
            "{:?}",
            info.notes
        );
        info.relocate(vec![0x1000, 0x2000]);
        assert_eq!(info.lookup("twice"), Some(0x100C));
        assert_eq!(info.lookup("value"), Some(0x2000));
        let file = info.find_file("/home/me/project/lines.s").expect("lines.s");
        // `moveq #0,d0` is the first instruction; its line is the first
        // row of the code hunk.
        let first = info.line_for(0x1000).expect("row at start");
        assert_eq!(first.file, file);
        let (_, addrs) = info.resolve_line(file, first.line).unwrap();
        assert_eq!(addrs, vec![0x1000]);
        // twice: is a label line with no code of its own; a breakpoint
        // there lands on the next line's instruction.
        let twice_hit = info.line_for(0x100C).unwrap();
        let (line, addrs) = info.resolve_line(file, twice_hit.line - 1).unwrap();
        assert_eq!(line, twice_hit.line);
        assert_eq!(addrs, vec![0x100C]);
        assert_eq!(info.line_extent(0x1002), Some((0x1002, 0x1004)));
    }
}
