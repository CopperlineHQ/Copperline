// SPDX-License-Identifier: GPL-3.0-or-later

//! Live AmigaOS ROM symbol discovery.
//!
//! Public function names come from a compact AROS-derived LVO table, but
//! addresses never do: the resolver walks Exec's live library/device lists
//! and reads each six-byte `JMP abs.l` vector. That follows SetFunction
//! patches and works with every Kickstart/AROS build. Resident tags give
//! module names and bounds for ROM code without a public library base.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::{OsList, OsMemory};

const LIB_NEG_SIZE: u32 = 16;
const JMP_ABS_LONG: u16 = 0x4EF9;
const VECTOR_SIZE: u32 = 6;
const VECTOR_CAP: u32 = 2048;

const RES_MODULES: u32 = 0x12C;
const RESLIST_JUMP: u32 = 0x8000_0000;
const RESIDENT_CAP: usize = 512;
const RTC_MATCHWORD: u16 = 0x4AFC;
const RT_MATCH_TAG: u32 = 2;
const RT_END_SKIP: u32 = 6;
const RT_FLAGS: u32 = 10;
const RT_VERSION: u32 = 11;
const RT_TYPE: u32 = 12;
const RT_PRI: u32 = 13;
const RT_NAME: u32 = 14;
const RT_ID_STRING: u32 = 18;
const RT_INIT: u32 = 22;
const RESIDENT_NAME_CAP: usize = 80;
const RESIDENT_ID_CAP: usize = 160;

/// Maximum inferred offset for a SetFunction target in RAM, where no
/// resident-tag boundary exists to prove a larger range.
const PATCHED_SYMBOL_SPAN: u32 = 0x1000;
/// Public entry points can contain substantial implementations, but a name
/// many kilobytes away is more misleading than the containing resident.
const ROM_SYMBOL_SPAN: u32 = 0x1000;
/// Modules without a bundled public table still get the standard management
/// vectors, but those names should cover only their immediate routines.
const GENERIC_VECTOR_SPAN: u32 = 0x100;

const LVO_TABLE: &str = include_str!("../../assets/symbols/amigaos-lvo.tsv");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RomRange {
    pub base: u32,
    pub size: u32,
}

impl RomRange {
    fn contains(&self, addr: u32) -> bool {
        addr.wrapping_sub(self.base) < self.size
    }

    fn contains_span(&self, start: u32, end: u32) -> bool {
        start >= self.base && end > start && end.wrapping_sub(self.base) <= self.size
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResidentModule {
    pub tag: u32,
    pub end: u32,
    pub name: String,
    pub id: String,
    pub flags: u8,
    pub version: u8,
    pub node_type: u8,
    pub priority: i8,
    pub init: u32,
}

impl ResidentModule {
    fn contains(&self, addr: u32) -> bool {
        (self.tag..self.end).contains(&addr)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LiveSymbol {
    pub address: u32,
    pub module: String,
    pub name: String,
    /// Positive vector number; its ABI byte offset is `-6 * lvo`.
    pub lvo: u16,
    pub vector: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedSymbol {
    pub address: u32,
    pub module: String,
    pub name: String,
    pub qualified: String,
    pub offset: u32,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lvo: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resident_tag: Option<u32>,
}

impl ResolvedSymbol {
    /// Compact debugger spelling used by DAP, the console and GDB.
    pub fn display_name(&self) -> String {
        let module = short_module_name(&self.module);
        if self.kind == "resident" {
            if self.offset == 0 {
                format!("[{module}]")
            } else {
                format!("[{module}]+${:X}", self.offset)
            }
        } else if self.offset == 0 {
            format!("[{module}] {}", self.name)
        } else {
            format!("[{module}] {}+${:X}", self.name, self.offset)
        }
    }

    /// Profile spelling; the prefix keeps ROM work grouped in flame charts.
    pub fn profile_name(&self) -> String {
        let module = short_module_name(&self.module);
        if self.kind == "resident" {
            format!("[Kick]{module}")
        } else {
            format!("[Kick]{module}/{}", self.name)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SymbolSnapshot {
    pub version: u32,
    pub rom_ranges: Vec<RomRange>,
    pub residents: Vec<ResidentModule>,
    pub symbols: Vec<LiveSymbol>,
}

impl Default for SymbolSnapshot {
    fn default() -> Self {
        Self {
            version: 1,
            rom_ranges: Vec::new(),
            residents: Vec::new(),
            symbols: Vec::new(),
        }
    }
}

impl SymbolSnapshot {
    pub fn is_rom_address(&self, addr: u32) -> bool {
        self.rom_ranges.iter().any(|range| range.contains(addr))
    }

    /// Resolve a PC to the nearest live LVO target inside the same resident
    /// module, falling back to the resident itself. SetFunction targets in
    /// RAM get a deliberately small inferred range.
    pub fn resolve(&self, addr: u32) -> Option<ResolvedSymbol> {
        let resident = self
            .residents
            .iter()
            .filter(|resident| resident.contains(addr))
            .min_by_key(|resident| resident.end - resident.tag);
        let mut best: Option<&LiveSymbol> = None;
        for symbol in &self.symbols {
            if symbol.address > addr {
                continue;
            }
            let offset = addr - symbol.address;
            let rom_span = if lvo_tables().contains_key(symbol.module.as_str()) {
                ROM_SYMBOL_SPAN
            } else {
                GENERIC_VECTOR_SPAN
            };
            let in_scope = match resident {
                Some(wanted) => wanted.contains(symbol.address) && offset <= rom_span,
                None => {
                    offset <= PATCHED_SYMBOL_SPAN
                        && !self
                            .residents
                            .iter()
                            .any(|candidate| candidate.contains(symbol.address))
                }
            };
            if !in_scope {
                continue;
            }
            if best.is_none_or(|current| {
                symbol.address > current.address
                    || (symbol.address == current.address && symbol.lvo < current.lvo)
            }) {
                best = Some(symbol);
            }
        }
        if let Some(symbol) = best {
            return Some(ResolvedSymbol {
                address: symbol.address,
                module: symbol.module.clone(),
                name: symbol.name.clone(),
                qualified: format!("{}/{}", symbol.module, symbol.name),
                offset: addr - symbol.address,
                kind: "lvo".into(),
                lvo: Some(-i32::from(symbol.lvo) * VECTOR_SIZE as i32),
                vector: Some(symbol.vector),
                resident_tag: resident.map(|module| module.tag),
            });
        }
        resident.map(|module| ResolvedSymbol {
            address: module.tag,
            module: module.name.clone(),
            name: module.name.clone(),
            qualified: module.name.clone(),
            offset: addr - module.tag,
            kind: "resident".into(),
            lvo: None,
            vector: None,
            resident_tag: Some(module.tag),
        })
    }
}

fn short_module_name(name: &str) -> &str {
    name.strip_suffix(".library")
        .or_else(|| name.strip_suffix(".device"))
        .or_else(|| name.strip_suffix(".resource"))
        .unwrap_or(name)
}

fn lvo_tables() -> &'static HashMap<&'static str, HashMap<u16, &'static str>> {
    static TABLES: OnceLock<HashMap<&'static str, HashMap<u16, &'static str>>> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut tables: HashMap<&str, HashMap<u16, &str>> = HashMap::new();
        for line in LVO_TABLE.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split('\t');
            let (Some(module), Some(lvo), Some(name), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let Ok(lvo) = lvo.parse() else {
                continue;
            };
            tables.entry(module).or_default().insert(lvo, name);
        }
        tables
    })
}

fn generic_vector_name(list: OsList, lvo: u16) -> Option<&'static str> {
    match (list, lvo) {
        (_, 1) => Some("Open"),
        (_, 2) => Some("Close"),
        (_, 3) => Some("Expunge"),
        (OsList::Devices, 5) => Some("BeginIO"),
        (OsList::Devices, 6) => Some("AbortIO"),
        _ => None,
    }
}

fn read_ascii(os: &OsMemory<'_>, ptr: u32, cap: usize) -> String {
    if ptr == 0 {
        return String::new();
    }
    let mut out = String::new();
    for i in 0..cap as u32 {
        let byte = (os.peek8)(ptr.wrapping_add(i));
        if byte == 0 || byte == b'\r' || byte == b'\n' {
            break;
        }
        if !(0x20..0x7F).contains(&byte) {
            return String::new();
        }
        out.push(byte as char);
    }
    out
}

fn resident_modules(
    os: &OsMemory<'_>,
    execbase: u32,
    rom_ranges: &[RomRange],
) -> Vec<ResidentModule> {
    let mut slot = (os.peek32)(execbase.wrapping_add(RES_MODULES));
    let mut seen_slots = HashSet::new();
    let mut residents = Vec::new();
    while slot != 0 && residents.len() < RESIDENT_CAP && seen_slots.insert(slot) {
        let entry = (os.peek32)(slot);
        if entry == 0 {
            break;
        }
        if entry & RESLIST_JUMP != 0 {
            slot = entry & !RESLIST_JUMP;
            continue;
        }
        if rom_ranges.iter().any(|range| range.contains(entry))
            && os.peek16(entry) == RTC_MATCHWORD
            && (os.peek32)(entry.wrapping_add(RT_MATCH_TAG)) == entry
        {
            let end = (os.peek32)(entry.wrapping_add(RT_END_SKIP));
            let range_ok = rom_ranges
                .iter()
                .any(|range| range.contains_span(entry, end));
            let name_ptr = (os.peek32)(entry.wrapping_add(RT_NAME));
            let name = read_ascii(os, name_ptr, RESIDENT_NAME_CAP);
            if range_ok && !name.is_empty() {
                residents.push(ResidentModule {
                    tag: entry,
                    end,
                    name,
                    id: read_ascii(
                        os,
                        (os.peek32)(entry.wrapping_add(RT_ID_STRING)),
                        RESIDENT_ID_CAP,
                    ),
                    flags: (os.peek8)(entry.wrapping_add(RT_FLAGS)),
                    version: (os.peek8)(entry.wrapping_add(RT_VERSION)),
                    node_type: (os.peek8)(entry.wrapping_add(RT_TYPE)),
                    priority: (os.peek8)(entry.wrapping_add(RT_PRI)) as i8,
                    init: (os.peek32)(entry.wrapping_add(RT_INIT)),
                });
            }
        }
        slot = slot.wrapping_add(4);
    }
    residents.sort_by_key(|resident| resident.tag);
    residents.dedup_by_key(|resident| resident.tag);
    residents
}

fn live_vectors(os: &OsMemory<'_>, execbase: u32) -> Vec<LiveSymbol> {
    let tables = lvo_tables();
    let mut symbols = Vec::new();
    for list in [OsList::Libraries, OsList::Devices] {
        for node in os.walk(execbase, list) {
            let table = tables.get(node.name.to_ascii_lowercase().as_str());
            let neg_size = u32::from(os.peek16(node.addr.wrapping_add(LIB_NEG_SIZE)));
            let entries = (neg_size / VECTOR_SIZE).min(VECTOR_CAP);
            for number in 1..=entries {
                let Ok(lvo) = u16::try_from(number) else {
                    break;
                };
                let name = table
                    .and_then(|table| table.get(&lvo).copied())
                    .or_else(|| generic_vector_name(list, lvo));
                let Some(name) = name else {
                    continue;
                };
                let vector = node.addr.wrapping_sub(number * VECTOR_SIZE);
                if os.peek16(vector) != JMP_ABS_LONG {
                    continue;
                }
                let address = (os.peek32)(vector.wrapping_add(2));
                if address == 0 || address & 1 != 0 {
                    continue;
                }
                symbols.push(LiveSymbol {
                    address,
                    module: node.name.clone(),
                    name: name.to_string(),
                    lvo,
                    vector,
                });
            }
        }
    }
    symbols.sort_by(|a, b| {
        a.address
            .cmp(&b.address)
            .then_with(|| a.module.cmp(&b.module))
            .then_with(|| a.lvo.cmp(&b.lvo))
    });
    symbols.dedup_by(|a, b| a.address == b.address && a.module == b.module && a.name == b.name);
    symbols
}

pub fn snapshot(os: &OsMemory<'_>, rom_ranges: Vec<RomRange>) -> SymbolSnapshot {
    let Ok(execbase) = os.exec_base() else {
        return SymbolSnapshot {
            rom_ranges,
            ..SymbolSnapshot::default()
        };
    };
    SymbolSnapshot {
        version: 1,
        residents: resident_modules(os, execbase, &rom_ranges),
        symbols: live_vectors(os, execbase),
        rom_ranges,
    }
}

/// Bus-backed snapshot shared by CCP, the built-in debugger and profiles.
pub fn snapshot_on_bus(bus: &crate::bus::Bus) -> SymbolSnapshot {
    let mut ranges = Vec::new();
    if !bus.mem.rom.is_empty() {
        ranges.push(RomRange {
            base: crate::memory::ROM_BASE as u32,
            size: bus.mem.rom.len() as u32,
        });
    }
    if !bus.mem.extended_rom.is_empty() {
        ranges.push(RomRange {
            base: bus.mem.extended_rom_base as u32,
            size: bus.mem.extended_rom.len() as u32,
        });
    }
    super::with_bus_memory(bus, |os| snapshot(os, ranges))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeMem(HashMap<u32, u8>);

    impl FakeMem {
        fn new() -> Self {
            Self(HashMap::new())
        }

        fn put8(&mut self, addr: u32, value: u8) {
            self.0.insert(addr, value);
        }

        fn put16(&mut self, addr: u32, value: u16) {
            for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
                self.put8(addr + i as u32, byte);
            }
        }

        fn put32(&mut self, addr: u32, value: u32) {
            for (i, byte) in value.to_be_bytes().into_iter().enumerate() {
                self.put8(addr + i as u32, byte);
            }
        }

        fn put_str(&mut self, addr: u32, value: &str) {
            for (i, byte) in value.bytes().chain(std::iter::once(0)).enumerate() {
                self.put8(addr + i as u32, byte);
            }
        }

        fn with_os<R>(&self, f: impl FnOnce(&OsMemory<'_>) -> R) -> R {
            let peek8 = |addr| self.0.get(&addr).copied().unwrap_or(0);
            let peek32 = |addr| {
                u32::from_be_bytes([
                    self.0.get(&addr).copied().unwrap_or(0),
                    self.0.get(&(addr + 1)).copied().unwrap_or(0),
                    self.0.get(&(addr + 2)).copied().unwrap_or(0),
                    self.0.get(&(addr + 3)).copied().unwrap_or(0),
                ])
            };
            let os = OsMemory {
                peek8: &peek8,
                peek32: &peek32,
            };
            f(&os)
        }

        fn exec(&mut self, base: u32) {
            self.put32(4, base);
            self.put32(base + super::super::CHKBASE, !base);
        }

        fn list(&mut self, exec: u32, offset: u32, nodes: &[u32]) {
            let header = exec + offset;
            self.put32(header, nodes.first().copied().unwrap_or(header + 4));
            for (index, node) in nodes.iter().copied().enumerate() {
                self.put32(node, nodes.get(index + 1).copied().unwrap_or(header + 4));
            }
            self.put32(header + 4, 0);
        }

        fn library(&mut self, base: u32, name_at: u32, name: &str, entries: u16) {
            self.put32(base + super::super::LN_NAME, name_at);
            self.put_str(name_at, name);
            self.put16(base + LIB_NEG_SIZE, entries * VECTOR_SIZE as u16);
        }

        fn vector(&mut self, base: u32, lvo: u32, target: u32) {
            let at = base - lvo * VECTOR_SIZE;
            self.put16(at, JMP_ABS_LONG);
            self.put32(at + 2, target);
        }

        fn resident(&mut self, tag: u32, end: u32, name_at: u32, name: &str) {
            self.put16(tag, RTC_MATCHWORD);
            self.put32(tag + RT_MATCH_TAG, tag);
            self.put32(tag + RT_END_SKIP, end);
            self.put32(tag + RT_NAME, name_at);
            self.put_str(name_at, name);
        }
    }

    #[test]
    fn live_exec_vector_names_follow_the_jump_target() {
        let mut mem = FakeMem::new();
        let (exec, lib, name) = (0x400, 0x800, 0xA00);
        mem.exec(exec);
        mem.library(lib, name, "exec.library", 40);
        mem.vector(lib, 33, 0x00F8_1234); // AllocMem
        mem.list(exec, super::super::LIB_LIST, &[lib]);
        mem.list(exec, super::super::DEVICE_LIST, &[]);
        let snapshot = mem.with_os(|os| {
            snapshot(
                os,
                vec![RomRange {
                    base: 0x00F8_0000,
                    size: 0x80000,
                }],
            )
        });
        let symbol = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.lvo == 33)
            .unwrap();
        assert_eq!(symbol.name, "AllocMem");
        assert_eq!(symbol.address, 0x00F8_1234);
    }

    #[test]
    fn resident_bounds_scope_nearest_symbol_resolution() {
        let snapshot = SymbolSnapshot {
            version: 1,
            rom_ranges: vec![RomRange {
                base: 0x00F8_0000,
                size: 0x80000,
            }],
            residents: vec![
                ResidentModule {
                    tag: 0x00F8_0100,
                    end: 0x00F8_2000,
                    name: "exec.library".into(),
                    id: String::new(),
                    flags: 0,
                    version: 40,
                    node_type: 9,
                    priority: 126,
                    init: 0,
                },
                ResidentModule {
                    tag: 0x00F8_2000,
                    end: 0x00F8_3000,
                    name: "other.device".into(),
                    id: String::new(),
                    flags: 0,
                    version: 1,
                    node_type: 3,
                    priority: 0,
                    init: 0,
                },
            ],
            symbols: vec![LiveSymbol {
                address: 0x00F8_1000,
                module: "exec.library".into(),
                name: "AllocMem".into(),
                lvo: 33,
                vector: 0x700,
            }],
        };
        assert_eq!(
            snapshot.resolve(0x00F8_1012).unwrap().display_name(),
            "[exec] AllocMem+$12"
        );
        let other = snapshot.resolve(0x00F8_2010).unwrap();
        assert_eq!(other.kind, "resident");
        assert_eq!(other.module, "other.device");
    }

    #[test]
    fn resident_list_jump_is_followed_and_out_of_rom_tags_are_rejected() {
        let mut mem = FakeMem::new();
        let (exec, list, tail) = (0x400, 0x600, 0x680);
        mem.exec(exec);
        mem.put32(exec + RES_MODULES, list);
        mem.put32(list, RESLIST_JUMP | tail);
        mem.put32(tail, 0x00F8_0100);
        mem.put32(tail + 4, 0x1000);
        mem.put32(tail + 8, 0);
        mem.resident(0x00F8_0100, 0x00F8_0200, 0x00F8_0180, "exec.library");
        mem.resident(0x1000, 0x1100, 0x1080, "ram.module");
        mem.list(exec, super::super::LIB_LIST, &[]);
        mem.list(exec, super::super::DEVICE_LIST, &[]);
        let snapshot = mem.with_os(|os| {
            snapshot(
                os,
                vec![RomRange {
                    base: 0x00F8_0000,
                    size: 0x80000,
                }],
            )
        });
        assert_eq!(snapshot.residents.len(), 1);
        assert_eq!(snapshot.residents[0].name, "exec.library");
    }

    #[test]
    fn required_abi_tables_are_bundled() {
        let tables = lvo_tables();
        for module in [
            "exec.library",
            "dos.library",
            "graphics.library",
            "intuition.library",
            "layers.library",
            "expansion.library",
            "utility.library",
            "icon.library",
            "diskfont.library",
            "keymap.library",
            "mathffp.library",
            "mathieeesingbas.library",
            "mathieeesingtrans.library",
            "mathieeedoubbas.library",
            "mathieeedoubtrans.library",
            "timer.device",
            "input.device",
            "console.device",
        ] {
            assert!(tables.contains_key(module), "missing {module}");
        }
    }
}
