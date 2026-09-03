// SPDX-License-Identifier: GPL-3.0-or-later

//! DWARF reading (through `gimli`) for the two places an Amiga program's
//! DWARF lives: the trailing debug hunk amiga-gcc `-g` writes, whose
//! sections are addressed from 0 per hunk, and an ELF the program was
//! converted from with elf2hunk, whose sections sit at the linker
//! script's addresses. Both come out as the same [`Loaded`] tables, with
//! addresses already turned into hunk + offset.
//!
//! Only the DWARF a C debugger needs at -O0 is read: line programs,
//! subprograms with their parameters and locals, lexical blocks, global
//! variables, the common type DIEs, and `.debug_frame` / `.eh_frame` for
//! unwinding. Variable locations are the single-operation forms
//! (`DW_OP_addr`, `DW_OP_fbreg`, `DW_OP_bregN`, `DW_OP_regN`,
//! `DW_OP_call_frame_cfa`); anything else is reported as unsupported
//! rather than guessed.

use super::hunk::DwarfBlock;
use super::{
    Encoding, Function, HunkAddr, HunkMeta, LineRow, LinkMap, LinkSegment, Location, Member,
    Symbol, TypeDesc, TypeId, Variable,
};
use gimli::{
    AttributeValue, BigEndian, DebuggingInformationEntry, Dwarf, EndianSlice, Reader, Unit,
    UnitOffset, UnwindSection,
};
use std::collections::HashMap;

type Slice<'a> = EndianSlice<'a, BigEndian>;

/// DIE nesting the tree walk follows before giving up on a subtree.
const MAX_DIE_DEPTH: usize = 128;

/// The tables one DWARF source yields, with file and type ids local to
/// this load (the caller remaps them).
#[derive(Debug, Default)]
pub struct Loaded {
    pub files: Vec<String>,
    pub rows: Vec<LineRow>,
    pub functions: Vec<Function>,
    pub globals: Vec<Variable>,
    pub types: Vec<TypeDesc>,
    pub cfi: Option<Cfi>,
    pub notes: Vec<String>,
}

/// Call-frame information, kept as section bytes and decoded per lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cfi {
    debug_frame: Vec<u8>,
    /// `.eh_frame` bytes and the section's link address.
    eh_frame: Option<(Vec<u8>, u64)>,
}

/// How to restore one register in the caller's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegRule {
    Undefined,
    SameValue,
    /// Saved at `CFA + n`.
    Offset(i64),
    /// The value is `CFA + n`.
    ValOffset(i64),
    /// Lives in another register.
    Register(u16),
    Unsupported,
}

/// The unwind row for one address: the CFA as register + offset, the
/// return-address column, and the saved registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnwindRow {
    pub cfa_reg: u16,
    pub cfa_offset: i64,
    pub ra_reg: u16,
    pub rules: Vec<(u16, RegRule)>,
}

impl Cfi {
    /// The unwind row covering link-time address `addr`.
    pub fn row_for(&self, addr: u64) -> Option<UnwindRow> {
        if !self.debug_frame.is_empty() {
            let mut section = gimli::DebugFrame::new(&self.debug_frame, BigEndian);
            section.set_address_size(4);
            let bases = gimli::BaseAddresses::default();
            if let Some(row) = unwind_row(&section, &bases, addr) {
                return Some(row);
            }
        }
        if let Some((bytes, at)) = &self.eh_frame {
            let mut section = gimli::EhFrame::new(bytes, BigEndian);
            section.set_address_size(4);
            let bases = gimli::BaseAddresses::default().set_eh_frame(*at);
            return unwind_row(&section, &bases, addr);
        }
        None
    }
}

fn unwind_row<'a, S>(section: &S, bases: &gimli::BaseAddresses, addr: u64) -> Option<UnwindRow>
where
    S: UnwindSection<Slice<'a>>,
{
    let fde = section
        .fde_for_address(bases, addr, S::cie_from_offset)
        .ok()?;
    let mut ctx = gimli::UnwindContext::new();
    let row = fde
        .unwind_info_for_address(section, bases, &mut ctx, addr)
        .ok()?;
    let (cfa_reg, cfa_offset) = match row.cfa() {
        gimli::CfaRule::RegisterAndOffset { register, offset } => (register.0, *offset),
        gimli::CfaRule::Expression(_) => return None,
    };
    let rules = row
        .registers()
        .map(|(reg, rule)| {
            let rule = match rule {
                gimli::RegisterRule::Undefined => RegRule::Undefined,
                gimli::RegisterRule::SameValue => RegRule::SameValue,
                gimli::RegisterRule::Offset(n) => RegRule::Offset(*n),
                gimli::RegisterRule::ValOffset(n) => RegRule::ValOffset(*n),
                gimli::RegisterRule::Register(r) => RegRule::Register(r.0),
                _ => RegRule::Unsupported,
            };
            (reg.0, rule)
        })
        .collect();
    Some(UnwindRow {
        cfa_reg,
        cfa_offset,
        ra_reg: fde.cie().return_address_register().0,
        rules,
    })
}

// ---------------------------------------------------------------------
// Sources

/// Read the DWARF in an executable's trailing debug hunk.
///
/// The amiga linker resolves every cross-section reference in the block
/// (`debug_abbrev_offset`, `DW_AT_stmt_list`, `DW_FORM_strp`, the FDEs'
/// CIE pointers) as an offset in the link address space, where the
/// block follows the program's hunks: a reference to byte `k` of
/// `.debug_line` reads `bias + line_start + k`, with `bias` the total
/// size of the hunks. Rather than rewrite the DWARF, each section is
/// handed to gimli behind that much leading padding, so the biased
/// references index it correctly; units are then enumerated from the
/// padded start of `.debug_info`, and the CIE pointers in a copy of
/// `.debug_frame` are un-biased (gimli walks that section from 0). The
/// bias is measured, not assumed: the first unit header's abbreviation
/// offset names where `.debug_abbrev` was placed.
///
/// Code and data addresses resolve through `link` (the hunks laid back
/// to back from 0); a global's `DW_OP_addr` prefers its hunk symbol.
pub fn load_block(
    block: &DwarfBlock,
    link: &LinkMap,
    symbols: &[Symbol],
) -> Result<Loaded, String> {
    let start_of = |name: &str| -> usize {
        block
            .sections
            .iter()
            .find(|(n, _)| n == name)
            .map_or(0, |(_, off)| *off as usize)
    };
    let info = block.section("info");
    let mut notes = Vec::new();
    let mut bias = measure_bias(info, start_of("abbrev"));
    if bias > block.data.len() {
        notes.push(format!(
            "debug block claims an abbreviation offset {bias} bytes past its own size; \
             reading it unbiased"
        ));
        bias = 0;
    }
    let owned: Vec<(&'static str, Vec<u8>)> = SECTION_NAMES
        .iter()
        .map(|&name| {
            let data = block.section(name);
            if data.is_empty() {
                return (name, Vec::new());
            }
            let mut padded = vec![0u8; bias + start_of(name)];
            padded.extend_from_slice(data);
            (name, padded)
        })
        .collect();
    let dwarf = load_dwarf(&owned)?;
    let addr_map = |addr: u64| link.to_hunk(addr);
    let static_map = |name: &str, addr: u64| {
        symbols
            .iter()
            .find(|s| s.name == name || s.name.strip_prefix('_') == Some(name))
            .map(|s| s.at)
            .or_else(|| link.to_hunk(addr))
    };
    let mut loaded = read_dwarf(&dwarf, &addr_map, &static_map, bias + start_of("info"))?;
    let frame = block.section("frame");
    if !frame.is_empty() {
        loaded.cfi = Some(Cfi {
            debug_frame: unbias_debug_frame(frame, bias + start_of("frame")),
            eh_frame: None,
        });
    }
    if bias != 0 {
        loaded.notes.push(format!("debug block bias {bias}"));
    }
    loaded.notes.extend(notes);
    Ok(loaded)
}

/// The bias of the block's cross-section references (see `load_block`):
/// the first unit header's `debug_abbrev_offset` less where
/// `.debug_abbrev` begins, when that is what it looks like.
fn measure_bias(info: &[u8], abbrev_start: usize) -> usize {
    if info.len() < 12 {
        return 0;
    }
    let u32_at = |o: usize| u32::from_be_bytes([info[o], info[o + 1], info[o + 2], info[o + 3]]);
    if u32_at(0) == 0xFFFF_FFFF {
        return 0; // 64-bit DWARF: not something the amiga linker writes
    }
    let version = u16::from_be_bytes([info[4], info[5]]);
    let abbrev_off = match version {
        2..=4 => u32_at(6),
        5 if info.len() >= 12 => u32_at(8),
        _ => return 0,
    } as usize;
    abbrev_off.saturating_sub(abbrev_start)
}

/// A copy of `.debug_frame` with the FDEs' CIE pointers made
/// section-relative again.
fn unbias_debug_frame(frame: &[u8], bias: usize) -> Vec<u8> {
    let mut out = frame.to_vec();
    if bias == 0 {
        return out;
    }
    let mut pos = 0usize;
    while pos + 8 <= out.len() {
        let length =
            u32::from_be_bytes([out[pos], out[pos + 1], out[pos + 2], out[pos + 3]]) as usize;
        if length == 0 || length == 0xFFFF_FFFF {
            break;
        }
        let id_at = pos + 4;
        let id = u32::from_be_bytes([out[id_at], out[id_at + 1], out[id_at + 2], out[id_at + 3]]);
        if id != 0xFFFF_FFFF {
            if let Some(fixed) = (id as usize).checked_sub(bias) {
                out[id_at..id_at + 4].copy_from_slice(&(fixed as u32).to_be_bytes());
            }
        }
        pos += 4 + length;
    }
    out
}

/// Read the DWARF of the ELF `elf2hunk` converted into a program whose
/// hunk table is `hunks`. Each allocatable, non-empty section became one
/// hunk in section order, so the link map follows from the section
/// headers.
pub fn load_elf(bytes: &[u8], hunks: &[HunkMeta]) -> Result<(Loaded, LinkMap), String> {
    use object::{Object as _, ObjectSection as _};
    let file = object::File::parse(bytes).map_err(|e| format!("not an ELF: {e}"))?;
    if file.is_little_endian() {
        return Err("little-endian ELF: not a 68k program".into());
    }
    let mut link = LinkMap::default();
    let mut notes = Vec::new();
    for section in file.sections() {
        let alloc = match section.flags() {
            object::SectionFlags::Elf { sh_flags, .. } => {
                (sh_flags & object::elf::SHF_ALLOC).0 != 0
            }
            _ => false,
        };
        if !alloc || section.size() == 0 {
            continue;
        }
        let hunk = link.segments.len() as u32;
        link.segments.push(LinkSegment {
            addr: section.address(),
            size: section.size(),
            hunk,
        });
    }
    if link.segments.len() != hunks.len() {
        notes.push(format!(
            "ELF has {} allocatable sections but the executable has {} hunks; addresses may \
             not line up",
            link.segments.len(),
            hunks.len()
        ));
    }
    let owned: Vec<(&'static str, Vec<u8>)> = SECTION_NAMES
        .iter()
        .map(|&name| {
            let data = file
                .section_by_name(&format!(".debug_{name}"))
                .and_then(|s| s.uncompressed_data().ok())
                .map(|d| d.into_owned())
                .unwrap_or_default();
            (name, data)
        })
        .collect();
    let dwarf = load_dwarf(&owned)?;
    let addr_map = |addr: u64| link.to_hunk(addr);
    let static_map = |_name: &str, addr: u64| link.to_hunk(addr);
    let mut loaded = read_dwarf(&dwarf, &addr_map, &static_map, 0)?;
    let frame = file
        .section_by_name(".debug_frame")
        .and_then(|s| s.uncompressed_data().ok())
        .map(|d| d.into_owned())
        .unwrap_or_default();
    let eh = file.section_by_name(".eh_frame").and_then(|s| {
        s.uncompressed_data()
            .ok()
            .map(|d| (d.into_owned(), s.address()))
    });
    if !frame.is_empty() || eh.as_ref().is_some_and(|(d, _)| !d.is_empty()) {
        loaded.cfi = Some(Cfi {
            debug_frame: frame,
            eh_frame: eh,
        });
    }
    loaded.notes.extend(notes);
    Ok((loaded, link))
}

const SECTION_NAMES: &[&str] = &[
    "abbrev",
    "addr",
    "aranges",
    "info",
    "line",
    "line_str",
    "loc",
    "loclists",
    "ranges",
    "rnglists",
    "str",
    "str_offsets",
    "types",
];

fn load_dwarf<'a>(owned: &'a [(&'static str, Vec<u8>)]) -> Result<Dwarf<Slice<'a>>, String> {
    let find = |id: gimli::SectionId| -> &'a [u8] {
        let want = id.name().trim_start_matches(".debug_");
        owned
            .iter()
            .find(|(name, _)| *name == want)
            .map_or(&[][..], |(_, data)| data.as_slice())
    };
    Dwarf::load(|id| Ok::<_, String>(EndianSlice::new(find(id), BigEndian)))
}

// ---------------------------------------------------------------------
// Reading

type AddrMap<'a> = dyn Fn(u64) -> Option<HunkAddr> + 'a;
type StaticMap<'a> = dyn Fn(&str, u64) -> Option<HunkAddr> + 'a;

/// What a DIE on the current path contributes to its descendants.
enum Frame {
    Function(usize),
    Block(Option<(HunkAddr, u32)>),
    Struct(TypeId),
    Enum(TypeId),
    Array(TypeId),
    Other,
}

struct Ctx<'d, 'a> {
    dwarf: &'d Dwarf<Slice<'a>>,
    addr_map: &'d AddrMap<'d>,
    static_map: &'d StaticMap<'d>,
    out: Loaded,
    /// (unit index, DIE offset) -> type id.
    type_ids: HashMap<(usize, usize), TypeId>,
    unit_index: usize,
    /// Line programs already read, so units sharing one do not
    /// duplicate its rows.
    line_programs: std::collections::HashSet<usize>,
}

impl<'d, 'a> Ctx<'d, 'a> {
    fn type_id(&mut self, offset: UnitOffset<usize>) -> TypeId {
        let key = (self.unit_index, offset.0);
        if let Some(&id) = self.type_ids.get(&key) {
            return id;
        }
        let id = self.out.types.len();
        self.out.types.push(TypeDesc::Unknown);
        self.type_ids.insert(key, id);
        id
    }

    fn string(&self, unit: &Unit<Slice<'a>>, value: AttributeValue<Slice<'a>>) -> Option<String> {
        self.dwarf
            .attr_string(unit, value)
            .ok()
            .map(|s| s.to_string_lossy().into_owned())
    }

    fn name(&self, unit: &Unit<Slice<'a>>, entry: &Die<'a>) -> Option<String> {
        let value = entry.attr_value(gimli::DW_AT_name)?;
        self.string(unit, value)
    }

    /// The name of `entry`, following `DW_AT_specification` /
    /// `DW_AT_abstract_origin` one hop.
    fn name_via_origin(&self, unit: &Unit<Slice<'a>>, entry: &Die<'a>) -> Option<String> {
        if let Some(name) = self.name(unit, entry) {
            return Some(name);
        }
        for at in [gimli::DW_AT_specification, gimli::DW_AT_abstract_origin] {
            if let Some(AttributeValue::UnitRef(off)) = entry.attr_value(at) {
                if let Ok(origin) = unit.entry(off) {
                    if let Some(name) = self.name(unit, &origin) {
                        return Some(name);
                    }
                }
            }
        }
        None
    }

    fn type_ref(&mut self, entry: &Die<'a>) -> Option<TypeId> {
        match entry.attr_value(gimli::DW_AT_type)? {
            AttributeValue::UnitRef(off) => Some(self.type_id(off)),
            _ => None,
        }
    }

    fn udata(entry: &Die<'a>, at: gimli::DwAt) -> Option<u64> {
        entry.attr_value(at)?.udata_value()
    }

    fn sdata(entry: &Die<'a>, at: gimli::DwAt) -> Option<i64> {
        match entry.attr_value(at)? {
            AttributeValue::Sdata(v) => Some(v),
            other => other.udata_value().and_then(|u| i64::try_from(u).ok()),
        }
    }

    /// `DW_AT_low_pc` / `DW_AT_high_pc` as a hunk range.
    fn pc_range(&self, entry: &Die<'a>) -> Option<(HunkAddr, u32)> {
        let low = match entry.attr_value(gimli::DW_AT_low_pc)? {
            AttributeValue::Addr(a) => a,
            _ => return None,
        };
        let high = match entry.attr_value(gimli::DW_AT_high_pc)? {
            AttributeValue::Addr(a) => a,
            other => low.checked_add(other.udata_value()?)?,
        };
        let at = (self.addr_map)(low)?;
        let size = u32::try_from(high.saturating_sub(low)).ok()?;
        Some((at, size))
    }

    /// A single-operation location expression.
    fn location(
        &self,
        unit: &Unit<Slice<'a>>,
        entry: &Die<'a>,
        at: gimli::DwAt,
        name: &str,
    ) -> Location {
        let Some(value) = entry.attr_value(at) else {
            return Location::Unsupported;
        };
        let AttributeValue::Exprloc(expr) = value else {
            return Location::Unsupported;
        };
        let mut reader = expr.0;
        let Ok(op) = gimli::Operation::parse(&mut reader, unit.encoding()) else {
            return Location::Unsupported;
        };
        if !reader.is_empty() {
            return Location::Unsupported;
        }
        match op {
            gimli::Operation::Address { address } => {
                (self.static_map)(name, address).map_or(Location::Unsupported, Location::Static)
            }
            gimli::Operation::FrameOffset { offset } => Location::FrameOffset(offset),
            gimli::Operation::RegisterOffset {
                register, offset, ..
            } => Location::RegOffset {
                reg: register.0,
                offset,
            },
            gimli::Operation::Register { register } => Location::Register(register.0),
            gimli::Operation::CallFrameCFA => Location::CallFrameCfa,
            _ => Location::Unsupported,
        }
    }

    fn variable(
        &mut self,
        unit: &Unit<Slice<'a>>,
        entry: &Die<'a>,
        scope: Option<(HunkAddr, u32)>,
    ) -> Option<Variable> {
        let name = self.name_via_origin(unit, entry)?;
        let ty = self.type_ref(entry);
        let location = self.location(unit, entry, gimli::DW_AT_location, &name);
        Some(Variable {
            name,
            ty,
            location,
            scope,
        })
    }

    fn read_lines(&mut self, unit: &Unit<Slice<'a>>) -> Result<(), String> {
        let Some(program) = unit.line_program.clone() else {
            return Ok(());
        };
        if !self.line_programs.insert(program.header().offset().0) {
            return Ok(());
        }
        let comp_dir = unit
            .comp_dir
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let comp_name = unit
            .name
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut file_ids: HashMap<u64, u32> = HashMap::new();
        let mut rows = program.rows();
        while let Some((header, row)) = rows.next_row().map_err(|e| e.to_string())? {
            let Some(at) = (self.addr_map)(row.address()) else {
                continue;
            };
            let index = row.file_index();
            let file = match file_ids.get(&index) {
                Some(&id) => id,
                None => {
                    let path = match header.file(index) {
                        Some(entry) => {
                            let name = self.string(unit, entry.path_name()).unwrap_or_default();
                            let dir = entry
                                .directory(header)
                                .and_then(|d| self.string(unit, d))
                                .unwrap_or_default();
                            join_path(&comp_dir, &dir, &name)
                        }
                        None => join_path(&comp_dir, "", &comp_name),
                    };
                    let id = intern(&mut self.out.files, &path);
                    file_ids.insert(index, id);
                    id
                }
            };
            let column = match row.column() {
                gimli::ColumnType::LeftEdge => 0,
                gimli::ColumnType::Column(c) => u32::try_from(c.get()).unwrap_or(0),
            };
            self.out.rows.push(LineRow {
                at,
                file,
                line: row
                    .line()
                    .map_or(0, |l| u32::try_from(l.get()).unwrap_or(0)),
                column,
                is_stmt: row.is_stmt(),
                end_sequence: row.end_sequence(),
            });
        }
        Ok(())
    }

    fn read_dies(&mut self, unit: &Unit<Slice<'a>>) -> Result<(), String> {
        let mut tree = unit.entries_tree(None).map_err(|e| e.to_string())?;
        let root = tree.root().map_err(|e| e.to_string())?;
        let mut stack: Vec<Frame> = Vec::new();
        self.walk(unit, root, &mut stack)
    }

    /// Depth-first over the DIE tree, with the path from the root on
    /// `stack` while a node's children are visited.
    fn walk(
        &mut self,
        unit: &Unit<Slice<'a>>,
        node: gimli::EntriesTreeNode<'_, '_, Slice<'a>>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), String> {
        // Real DWARF nests a handful of levels; a crafted tree could
        // nest as deep as the section is long.
        if stack.len() >= MAX_DIE_DEPTH {
            return Ok(());
        }
        let frame = self.visit(unit, node.entry(), stack);
        stack.push(frame);
        let mut children = node.children();
        while let Some(child) = children.next().map_err(|e| e.to_string())? {
            self.walk(unit, child, stack)?;
        }
        stack.pop();
        Ok(())
    }

    fn visit(&mut self, unit: &Unit<Slice<'a>>, entry: &Die<'a>, stack: &[Frame]) -> Frame {
        let function = stack.iter().rev().find_map(|f| match f {
            Frame::Function(i) => Some(*i),
            _ => None,
        });
        let scope = stack.iter().rev().find_map(|f| match f {
            Frame::Block(range) => Some(*range),
            Frame::Function(_) => Some(None),
            _ => None,
        });
        match entry.tag() {
            gimli::DW_TAG_subprogram => {
                let Some((at, size)) = self.pc_range(entry) else {
                    return Frame::Other;
                };
                let name = self
                    .name_via_origin(unit, entry)
                    .unwrap_or_else(|| format!("fn_{:x}", entry.offset().0));
                let frame_base = self.location(unit, entry, gimli::DW_AT_frame_base, &name);
                let file = Self::udata(entry, gimli::DW_AT_decl_file);
                let line = Self::udata(entry, gimli::DW_AT_decl_line);
                let file = file.and_then(|index| self.decl_file(unit, index));
                self.out.functions.push(Function {
                    name,
                    at,
                    size,
                    frame_base,
                    params: Vec::new(),
                    locals: Vec::new(),
                    file,
                    line: line.and_then(|l| u32::try_from(l).ok()),
                });
                Frame::Function(self.out.functions.len() - 1)
            }
            gimli::DW_TAG_lexical_block => Frame::Block(self.pc_range(entry)),
            gimli::DW_TAG_formal_parameter => {
                if let (Some(f), Some(v)) = (function, self.variable(unit, entry, None)) {
                    self.out.functions[f].params.push(v);
                }
                Frame::Other
            }
            gimli::DW_TAG_variable => {
                match function {
                    Some(f) => {
                        if let Some(v) = self.variable(unit, entry, scope.flatten()) {
                            self.out.functions[f].locals.push(v);
                        }
                    }
                    None => {
                        // A declaration without a location is not a
                        // variable we can show; a definition elsewhere
                        // in the unit carries the address.
                        if let Some(v) = self.variable(unit, entry, None) {
                            if v.location != Location::Unsupported {
                                self.out.globals.push(v);
                            }
                        }
                    }
                }
                Frame::Other
            }
            gimli::DW_TAG_base_type => {
                let id = self.type_id(entry.offset());
                let name = self.name(unit, entry).unwrap_or_else(|| "?".into());
                let size = byte_size(entry, 0);
                let encoding = match entry.attr_value(gimli::DW_AT_encoding) {
                    Some(AttributeValue::Encoding(gimli::DW_ATE_float)) => Encoding::Float,
                    Some(AttributeValue::Encoding(gimli::DW_ATE_boolean)) => Encoding::Bool,
                    Some(AttributeValue::Encoding(gimli::DW_ATE_signed_char)) => {
                        Encoding::SignedChar
                    }
                    Some(AttributeValue::Encoding(gimli::DW_ATE_unsigned_char)) => {
                        Encoding::UnsignedChar
                    }
                    Some(AttributeValue::Encoding(gimli::DW_ATE_unsigned)) => Encoding::Unsigned,
                    _ => Encoding::Signed,
                };
                self.out.types[id] = TypeDesc::Base {
                    name,
                    size,
                    encoding,
                };
                Frame::Other
            }
            gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type => {
                let id = self.type_id(entry.offset());
                let target = self.type_ref(entry);
                let size = byte_size(entry, 4);
                self.out.types[id] = TypeDesc::Pointer { target, size };
                Frame::Other
            }
            gimli::DW_TAG_array_type => {
                let id = self.type_id(entry.offset());
                let element = self.type_ref(entry);
                self.out.types[id] = TypeDesc::Array {
                    element,
                    count: None,
                };
                Frame::Array(id)
            }
            gimli::DW_TAG_subrange_type => {
                if let Some(Frame::Array(id)) = stack.last() {
                    let count = Self::udata(entry, gimli::DW_AT_count).or_else(|| {
                        Self::udata(entry, gimli::DW_AT_upper_bound)
                            .and_then(|ub| ub.checked_add(1))
                    });
                    if let TypeDesc::Array { count: slot, .. } = &mut self.out.types[*id] {
                        *slot = count;
                    }
                }
                Frame::Other
            }
            gimli::DW_TAG_structure_type | gimli::DW_TAG_union_type | gimli::DW_TAG_class_type => {
                let id = self.type_id(entry.offset());
                let name = self.name(unit, entry);
                let size = byte_size(entry, 0);
                self.out.types[id] = TypeDesc::Struct {
                    name,
                    size,
                    members: Vec::new(),
                    is_union: entry.tag() == gimli::DW_TAG_union_type,
                };
                Frame::Struct(id)
            }
            gimli::DW_TAG_member => {
                if let Some(Frame::Struct(id)) = stack.last() {
                    let id = *id;
                    let name = self.name(unit, entry).unwrap_or_default();
                    let ty = self.type_ref(entry);
                    let offset = self.member_offset(unit, entry);
                    if let TypeDesc::Struct { members, .. } = &mut self.out.types[id] {
                        members.push(Member { name, ty, offset });
                    }
                }
                Frame::Other
            }
            gimli::DW_TAG_enumeration_type => {
                let id = self.type_id(entry.offset());
                let name = self.name(unit, entry);
                let size = byte_size(entry, 4);
                self.out.types[id] = TypeDesc::Enum {
                    name,
                    size,
                    values: Vec::new(),
                };
                Frame::Enum(id)
            }
            gimli::DW_TAG_enumerator => {
                if let Some(Frame::Enum(id)) = stack.last() {
                    let id = *id;
                    let name = self.name(unit, entry).unwrap_or_default();
                    let value = Self::sdata(entry, gimli::DW_AT_const_value).unwrap_or(0);
                    if let TypeDesc::Enum { values, .. } = &mut self.out.types[id] {
                        values.push((name, value));
                    }
                }
                Frame::Other
            }
            gimli::DW_TAG_typedef => {
                let id = self.type_id(entry.offset());
                let name = self.name(unit, entry).unwrap_or_else(|| "?".into());
                let target = self.type_ref(entry);
                self.out.types[id] = TypeDesc::Typedef { name, target };
                Frame::Other
            }
            gimli::DW_TAG_const_type | gimli::DW_TAG_volatile_type => {
                let id = self.type_id(entry.offset());
                let target = self.type_ref(entry);
                let qualifier = if entry.tag() == gimli::DW_TAG_const_type {
                    "const"
                } else {
                    "volatile"
                };
                self.out.types[id] = TypeDesc::Qualified { qualifier, target };
                Frame::Other
            }
            gimli::DW_TAG_subroutine_type => {
                let id = self.type_id(entry.offset());
                self.out.types[id] = TypeDesc::Function;
                Frame::Other
            }
            gimli::DW_TAG_unspecified_type => {
                let id = self.type_id(entry.offset());
                self.out.types[id] = TypeDesc::Void;
                Frame::Other
            }
            _ => Frame::Other,
        }
    }

    /// `DW_AT_data_member_location` as a byte offset.
    fn member_offset(&self, unit: &Unit<Slice<'a>>, entry: &Die<'a>) -> u32 {
        match entry.attr_value(gimli::DW_AT_data_member_location) {
            Some(AttributeValue::Exprloc(expr)) => {
                let mut reader = expr.0;
                match gimli::Operation::parse(&mut reader, unit.encoding()) {
                    Ok(gimli::Operation::PlusConstant { value }) => {
                        u32::try_from(value).unwrap_or(0)
                    }
                    _ => 0,
                }
            }
            Some(other) => other
                .udata_value()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0),
            None => 0,
        }
    }

    /// A `DW_AT_decl_file` index as one of our file ids.
    fn decl_file(&mut self, unit: &Unit<Slice<'a>>, index: u64) -> Option<u32> {
        let program = unit.line_program.as_ref()?;
        let header = program.header();
        let entry = header.file(index)?;
        let name = self.string(unit, entry.path_name()).unwrap_or_default();
        let dir = entry
            .directory(header)
            .and_then(|d| self.string(unit, d))
            .unwrap_or_default();
        let comp_dir = unit
            .comp_dir
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = join_path(&comp_dir, &dir, &name);
        Some(intern(&mut self.out.files, &path))
    }
}

type Die<'a> = DebuggingInformationEntry<Slice<'a>>;

/// Read every unit of `.debug_info` from `first_unit` on.
fn read_dwarf(
    dwarf: &Dwarf<Slice<'_>>,
    addr_map: &AddrMap<'_>,
    static_map: &StaticMap<'_>,
    first_unit: usize,
) -> Result<Loaded, String> {
    let mut ctx = Ctx {
        dwarf,
        addr_map,
        static_map,
        out: Loaded::default(),
        type_ids: HashMap::new(),
        unit_index: 0,
        line_programs: std::collections::HashSet::new(),
    };
    let mut offset = first_unit;
    let end = gimli::Section::reader(&dwarf.debug_info).len();
    let mut unit_index = 0usize;
    while offset + 11 < end {
        let header = dwarf
            .debug_info
            .header_from_offset(gimli::DebugInfoOffset(offset))
            .map_err(|e| e.to_string())?;
        let next = offset + header.length_including_self();
        let unit = dwarf.unit(header).map_err(|e| e.to_string())?;
        ctx.unit_index = unit_index;
        unit_index += 1;
        ctx.read_lines(&unit)?;
        ctx.read_dies(&unit)?;
        if next <= offset {
            break;
        }
        offset = next;
    }
    if unit_index == 0 {
        return Err("no compilation units".into());
    }
    Ok(ctx.out)
}

/// `DW_AT_byte_size`, or `default`; a size past 32 bits is no size.
fn byte_size(entry: &Die<'_>, default: u32) -> u32 {
    entry
        .attr_value(gimli::DW_AT_byte_size)
        .and_then(|v| v.udata_value())
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(default)
}

fn intern(files: &mut Vec<String>, path: &str) -> u32 {
    if let Some(i) = files.iter().position(|f| f == path) {
        return i as u32;
    }
    files.push(path.to_string());
    (files.len() - 1) as u32
}

/// `comp_dir` + `dir` + `name`, honouring absolute components.
fn join_path(comp_dir: &str, dir: &str, name: &str) -> String {
    let name = super::normalize_path(name);
    if name.starts_with('/') || name.chars().nth(1) == Some(':') {
        return name;
    }
    let dir = super::normalize_path(dir);
    let base = if dir.starts_with('/') || dir.chars().nth(1) == Some(':') {
        dir
    } else if dir.is_empty() {
        super::normalize_path(comp_dir)
    } else if comp_dir.is_empty() {
        dir
    } else {
        format!(
            "{}/{dir}",
            super::normalize_path(comp_dir).trim_end_matches('/')
        )
    };
    if base.is_empty() {
        name
    } else {
        format!("{}/{name}", base.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_join_with_absolute_components_winning() {
        assert_eq!(join_path("/build", "", "a.c"), "/build/a.c");
        assert_eq!(join_path("/build", "src", "a.c"), "/build/src/a.c");
        assert_eq!(join_path("/build", "/inc", "a.h"), "/inc/a.h");
        assert_eq!(join_path("/build", "src", "/abs/a.c"), "/abs/a.c");
        assert_eq!(join_path("", "", "a.c"), "a.c");
        assert_eq!(join_path("C:\\w", "", "a.c"), "C:/w/a.c");
    }
}
