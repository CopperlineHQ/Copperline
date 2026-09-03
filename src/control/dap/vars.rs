// SPDX-License-Identifier: GPL-3.0-or-later

//! Scopes, variables and values: the register file, the DWARF locals
//! and globals of the program, the custom chipset, and the typed value
//! rendering behind hovers and the Variables view. Guest memory comes
//! through a chunked cache over `mem.read`, refilled per stop.

use super::eval;
use crate::control::bridge::{Bridge, Reply};
use crate::control::proto;
use crate::debuginfo::unwind::{Frame, Registers};
use crate::debuginfo::{
    DebugInfo, Encoding, Function, HunkAddr, Location, TypeDesc, TypeId, Variable,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Guest memory read through the control protocol, cached in aligned
/// chunks for the duration of one request.
pub struct GuestMem<'a> {
    bridge: &'a Bridge,
    chunks: HashMap<u32, Option<Vec<u8>>>,
}

const CHUNK: u32 = 256;

impl<'a> GuestMem<'a> {
    pub fn new(bridge: &'a Bridge) -> Self {
        Self {
            bridge,
            chunks: HashMap::new(),
        }
    }

    fn chunk(&mut self, base: u32) -> Option<&[u8]> {
        if !self.chunks.contains_key(&base) {
            let fetched = match self.bridge.call(
                "mem.read",
                json!({"addr": base, "len": CHUNK, "encoding": "base64"}),
            ) {
                Ok(Reply::Ok(v)) => v["data"]
                    .as_str()
                    .and_then(proto::decode_base64)
                    .filter(|d| d.len() == CHUNK as usize),
                _ => None,
            };
            self.chunks.insert(base, fetched);
        }
        self.chunks.get(&base).and_then(|c| c.as_deref())
    }

    pub fn read(&mut self, addr: u32, len: usize) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        let mut at = addr;
        while out.len() < len {
            let base = at & !(CHUNK - 1);
            let off = (at - base) as usize;
            let take = (len - out.len()).min(CHUNK as usize - off);
            let chunk = self.chunk(base)?;
            out.extend_from_slice(&chunk[off..off + take]);
            at = at.wrapping_add(take as u32);
        }
        Some(out)
    }

    pub fn read8(&mut self, addr: u32) -> Option<u8> {
        self.read(addr, 1).map(|b| b[0])
    }

    pub fn read16(&mut self, addr: u32) -> Option<u16> {
        self.read(addr, 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn read32(&mut self, addr: u32) -> Option<u32> {
        self.read(addr, 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// What a `variablesReference` stands for.
#[derive(Clone, Debug)]
pub enum Node {
    Registers {
        frame: usize,
    },
    StatusRegister,
    Locals {
        frame: usize,
    },
    Globals,
    Chipset,
    /// A typed value in memory whose members / elements / target are
    /// its children.
    Typed {
        addr: u32,
        ty: TypeId,
        frame: usize,
    },
}

/// Live `variablesReference`s, cleared at every stop.
#[derive(Default)]
pub struct VarStore {
    nodes: Vec<Node>,
}

impl VarStore {
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    pub fn add(&mut self, node: Node) -> i64 {
        self.nodes.push(node);
        self.nodes.len() as i64
    }

    pub fn get(&self, reference: i64) -> Option<&Node> {
        usize::try_from(reference)
            .ok()
            .and_then(|r| r.checked_sub(1))
            .and_then(|i| self.nodes.get(i))
    }
}

/// The `scopes` reply for a frame.
pub fn scopes(store: &mut VarStore, frame: usize, has_info: bool) -> Value {
    let mut out = vec![json!({
        "name": "Registers",
        "presentationHint": "registers",
        "variablesReference": store.add(Node::Registers { frame }),
        "expensive": false,
    })];
    if has_info {
        out.push(json!({
            "name": "Locals",
            "presentationHint": "locals",
            "variablesReference": store.add(Node::Locals { frame }),
            "expensive": false,
        }));
        out.push(json!({
            "name": "Globals",
            "variablesReference": store.add(Node::Globals),
            "expensive": true,
        }));
    }
    out.push(json!({
        "name": "Chipset",
        "variablesReference": store.add(Node::Chipset),
        "expensive": true,
    }));
    json!({"scopes": out})
}

fn hex32(v: u32) -> String {
    format!("0x{v:08X}")
}

/// The register file of a frame as variables.
pub fn registers(regs: &Registers, live: bool) -> Vec<Value> {
    let mut out = Vec::new();
    let hint = |kind: &str| json!({"kind": kind, "attributes": if live { json!([]) } else { json!(["readOnly"]) }});
    for i in 0..8 {
        out.push(json!({
            "name": format!("d{i}"),
            "value": format!("{} ({})", hex32(regs.d[i]), regs.d[i] as i32),
            "type": "long",
            "evaluateName": format!("d{i}"),
            "variablesReference": 0,
            "presentationHint": hint("data"),
        }));
    }
    for i in 0..8 {
        out.push(json!({
            "name": format!("a{i}"),
            "value": hex32(regs.a[i]),
            "type": "address",
            "evaluateName": format!("a{i}"),
            "variablesReference": 0,
            "memoryReference": format!("0x{:X}", regs.a[i]),
            "presentationHint": hint("data"),
        }));
    }
    out.push(json!({
        "name": "pc",
        "value": hex32(regs.pc),
        "type": "address",
        "evaluateName": "pc",
        "variablesReference": 0,
        "memoryReference": format!("0x{:X}", regs.pc),
        "presentationHint": hint("data"),
    }));
    out
}

/// The status register, live only (frame 0).
pub fn status_register(store: &mut VarStore, sr: u16) -> Value {
    json!({
        "name": "sr",
        "value": format!("0x{sr:04X} [{}]", sr_flags(sr)),
        "type": "word",
        "evaluateName": "sr",
        "variablesReference": store.add(Node::StatusRegister),
    })
}

pub fn sr_flags(sr: u16) -> String {
    let mut s = String::new();
    for (bit, name) in [(15, 'T'), (13, 'S')] {
        if sr & (1 << bit) != 0 {
            s.push(name);
        }
    }
    s.push_str(&format!("I{}", (sr >> 8) & 7));
    for (bit, name) in [(4, 'X'), (3, 'N'), (2, 'Z'), (1, 'V'), (0, 'C')] {
        s.push(if sr & (1 << bit) != 0 { name } else { '-' });
    }
    s
}

pub fn status_register_bits(sr: u16) -> Vec<Value> {
    let bit = |name: &str, on: bool| json!({"name": name, "value": if on { "1" } else { "0" }, "variablesReference": 0});
    vec![
        bit("T (trace)", sr & 0x8000 != 0),
        bit("S (supervisor)", sr & 0x2000 != 0),
        json!({"name": "IPL", "value": ((sr >> 8) & 7).to_string(), "variablesReference": 0}),
        bit("X", sr & 0x10 != 0),
        bit("N", sr & 0x08 != 0),
        bit("Z", sr & 0x04 != 0),
        bit("V", sr & 0x02 != 0),
        bit("C", sr & 0x01 != 0),
    ]
}

/// The chipset scope: every custom register from `custom.dump` and the
/// beam position.
pub fn chipset(bridge: &Bridge) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(Reply::Ok(beam)) = bridge.call("beam.get", json!({})) {
        for key in ["frame", "vpos", "hpos", "cck", "seconds"] {
            if let Some(v) = beam.get(key) {
                out.push(json!({"name": key, "value": v.to_string(), "variablesReference": 0}));
            }
        }
    }
    if let Ok(Reply::Ok(dump)) = bridge.call("custom.dump", json!({})) {
        if let Some(regs) = dump["regs"].as_object() {
            for (name, value) in regs {
                let v = value.as_u64().unwrap_or(0);
                out.push(json!({
                    "name": name,
                    "value": format!("0x{v:04X}"),
                    "variablesReference": 0,
                    "presentationHint": {"attributes": ["readOnly"]},
                }));
            }
        }
    }
    out
}

/// Where a variable's storage is in one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Memory(u32),
    Register(u16),
}

/// The canonical frame address of `frame`, from the CFI row at its PC.
pub fn frame_cfa(info: &DebugInfo, frame: &Frame) -> Option<u32> {
    let at = info.locate(frame.pc)?;
    let link = info.link.to_link(at)?;
    let row = info.cfi.as_ref()?.row_for(link)?;
    let base = i64::from(frame.regs.get(row.cfa_reg)?);
    u32::try_from(base + row.cfa_offset).ok()
}

fn frame_base(info: &DebugInfo, frame: &Frame, function: Option<&Function>) -> Result<u32, String> {
    let function = function.ok_or("no enclosing function")?;
    match &function.frame_base {
        Location::RegOffset { reg, offset } => {
            let base = frame.regs.get(*reg).ok_or("frame base register unknown")?;
            Ok((i64::from(base) + offset) as u32)
        }
        Location::Register(reg) => frame
            .regs
            .get(*reg)
            .ok_or_else(|| "frame base register unknown".into()),
        Location::CallFrameCfa => {
            frame_cfa(info, frame).ok_or_else(|| "no call-frame information here".into())
        }
        Location::FrameOffset(_) | Location::Static(_) | Location::Unsupported => {
            Err("unsupported frame base".into())
        }
    }
}

pub fn place_of(
    info: &DebugInfo,
    frame: &Frame,
    function: Option<&Function>,
    location: &Location,
) -> Result<Place, String> {
    match location {
        Location::Static(at) => info
            .runtime(*at)
            .map(Place::Memory)
            .ok_or_else(|| "not relocated".into()),
        Location::FrameOffset(off) => {
            let base = frame_base(info, frame, function)?;
            Ok(Place::Memory((i64::from(base) + off) as u32))
        }
        Location::RegOffset { reg, offset } => {
            let base = frame
                .regs
                .get(*reg)
                .ok_or("register unknown in this frame")?;
            Ok(Place::Memory((i64::from(base) + offset) as u32))
        }
        Location::Register(reg) => Ok(Place::Register(*reg)),
        Location::CallFrameCfa => frame_cfa(info, frame)
            .map(Place::Memory)
            .ok_or_else(|| "no call-frame information here".into()),
        Location::Unsupported => Err("location not supported".into()),
    }
}

/// A rendered value and, when it has children, their node.
pub struct Rendered {
    pub value: String,
    pub ty: String,
    pub children: Option<Node>,
    pub memory: Option<u32>,
}

/// Render the value of type `ty` at `addr`.
pub fn render(
    info: &DebugInfo,
    mem: &mut GuestMem,
    ty: Option<TypeId>,
    addr: u32,
    frame: usize,
) -> Rendered {
    let ty_name = info.type_name(ty);
    let resolved = info.resolve_type(ty);
    let Some(desc) = resolved.and_then(|id| info.types.get(id)) else {
        let v = mem.read32(addr).map_or("<unreadable>".into(), hex32);
        return Rendered {
            value: v,
            ty: ty_name,
            children: None,
            memory: Some(addr),
        };
    };
    let mut rendered = Rendered {
        value: String::new(),
        ty: ty_name,
        children: None,
        memory: Some(addr),
    };
    match desc {
        TypeDesc::Base { size, encoding, .. } => {
            rendered.value = render_base(mem, addr, *size, *encoding);
        }
        TypeDesc::Pointer { target, .. } => {
            let Some(ptr) = mem.read32(addr) else {
                rendered.value = "<unreadable>".into();
                return rendered;
            };
            rendered.value = format!("0x{ptr:X}");
            let target_desc = info.resolve_type(*target).and_then(|t| info.types.get(t));
            if ptr == 0 {
                // NULL: nothing to look through.
            } else if let Some(TypeDesc::Base { size: 1, .. }) = target_desc {
                if let Some(s) = read_c_string(mem, ptr, 64) {
                    rendered.value = format!("0x{ptr:X} {s:?}");
                }
            } else if ptr != 0
                && target.is_some()
                && !matches!(target_desc, Some(TypeDesc::Void) | None)
            {
                rendered.children = Some(Node::Typed {
                    addr: ptr,
                    ty: target.expect("checked"),
                    frame,
                });
            }
        }
        TypeDesc::Array { element, count } => {
            rendered.value = match count {
                Some(n) => format!("[{n}] {}", info.type_name(*element)),
                None => format!("[] {}", info.type_name(*element)),
            };
            if let Some(TypeDesc::Base { size: 1, .. }) =
                info.resolve_type(*element).and_then(|t| info.types.get(t))
            {
                if let Some(s) = read_c_string(mem, addr, count.map_or(64, |n| n.min(64) as usize))
                {
                    rendered.value = format!("{s:?}");
                }
            }
            if count.is_some() {
                rendered.children = Some(Node::Typed {
                    addr,
                    ty: resolved.expect("checked"),
                    frame,
                });
            }
        }
        TypeDesc::Struct { members, .. } => {
            let mut parts = Vec::new();
            for m in members.iter().take(4) {
                let inner = render(info, mem, m.ty, addr.wrapping_add(m.offset), frame);
                parts.push(format!("{}: {}", m.name, inner.value));
            }
            if members.len() > 4 {
                parts.push("...".into());
            }
            rendered.value = format!("{{{}}}", parts.join(", "));
            rendered.children = Some(Node::Typed {
                addr,
                ty: resolved.expect("checked"),
                frame,
            });
        }
        TypeDesc::Enum { size, values, .. } => {
            let raw = read_signed(mem, addr, *size);
            rendered.value = match raw {
                Some(v) => values
                    .iter()
                    .find(|(_, x)| *x == v)
                    .map_or(v.to_string(), |(name, _)| format!("{name} ({v})")),
                None => "<unreadable>".into(),
            };
        }
        TypeDesc::Function | TypeDesc::Void | TypeDesc::Unknown => {
            rendered.value = mem.read32(addr).map_or("<unreadable>".into(), hex32);
        }
        TypeDesc::Typedef { .. } | TypeDesc::Qualified { .. } => unreachable!("resolved"),
    }
    rendered
}

fn read_signed(mem: &mut GuestMem, addr: u32, size: u32) -> Option<i64> {
    Some(match size {
        1 => i64::from(mem.read8(addr)? as i8),
        2 => i64::from(mem.read16(addr)? as i16),
        _ => i64::from(mem.read32(addr)? as i32),
    })
}

fn render_base(mem: &mut GuestMem, addr: u32, size: u32, encoding: Encoding) -> String {
    let Some(bytes) = mem.read(addr, size.clamp(1, 16) as usize) else {
        return "<unreadable>".into();
    };
    match (encoding, size) {
        (Encoding::Float, 4) => {
            f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).to_string()
        }
        (Encoding::Float, 8) => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[..8]);
            f64::from_be_bytes(b).to_string()
        }
        (Encoding::Float, _) => format!("0x{}", proto::encode_hex(&bytes)),
        (Encoding::Bool, _) => (bytes.iter().any(|b| *b != 0)).to_string(),
        (Encoding::SignedChar | Encoding::UnsignedChar, 1) => {
            let c = bytes[0];
            let shown = if encoding == Encoding::SignedChar {
                i64::from(c as i8)
            } else {
                i64::from(c)
            };
            if (0x20..0x7F).contains(&c) {
                format!("{shown} '{}'", c as char)
            } else {
                format!("{shown}")
            }
        }
        (Encoding::Signed | Encoding::SignedChar, _) => {
            let v = match size {
                1 => i64::from(bytes[0] as i8),
                2 => i64::from(i16::from_be_bytes([bytes[0], bytes[1]])),
                4 => i64::from(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                _ => i64::from_be_bytes(low_eight(&bytes)),
            };
            v.to_string()
        }
        (Encoding::Unsigned | Encoding::UnsignedChar, _) => {
            let v = match size {
                1 => u64::from(bytes[0]),
                2 => u64::from(u16::from_be_bytes([bytes[0], bytes[1]])),
                4 => u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
                _ => u64::from_be_bytes(low_eight(&bytes)),
            };
            format!("{v} (0x{v:X})")
        }
    }
}

/// The least significant eight bytes of a big-endian value of any
/// size (an odd-sized or oversized base type still renders something).
fn low_eight(bytes: &[u8]) -> [u8; 8] {
    let mut b = [0u8; 8];
    let n = bytes.len().min(8);
    b[8 - n..].copy_from_slice(&bytes[bytes.len() - n..]);
    b
}

fn read_c_string(mem: &mut GuestMem, addr: u32, cap: usize) -> Option<String> {
    let bytes = mem.read(addr, cap)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text: String = bytes[..end]
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) || b == b'\n' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    Some(text)
}

/// One variable of a scope as a DAP `Variable`.
pub fn variable_value(
    info: &DebugInfo,
    mem: &mut GuestMem,
    store: &mut VarStore,
    name: &str,
    place: Result<Place, String>,
    ty: Option<TypeId>,
    frame: &Frame,
    frame_index: usize,
) -> Value {
    match place {
        Ok(Place::Memory(addr)) => {
            let r = render(info, mem, ty, addr, frame_index);
            let mut v = json!({
                "name": name,
                "value": r.value,
                "type": r.ty,
                "evaluateName": name,
                "variablesReference": r.children.map_or(0, |n| store.add(n)),
            });
            if let Some(addr) = r.memory {
                v["memoryReference"] = Value::from(format!("0x{addr:X}"));
            }
            v
        }
        Ok(Place::Register(reg)) => {
            let value = frame.regs.get(reg).map_or("<unknown>".into(), hex32);
            json!({
                "name": name,
                "value": format!("{value} (in {})", eval::register_name(reg)),
                "type": info.type_name(ty),
                "evaluateName": name,
                "variablesReference": 0,
            })
        }
        Err(why) => json!({
            "name": name,
            "value": format!("<{why}>"),
            "type": info.type_name(ty),
            "variablesReference": 0,
        }),
    }
}

/// The children of a typed node: struct members, array elements, or a
/// pointer's target.
pub fn typed_children(
    info: &DebugInfo,
    mem: &mut GuestMem,
    store: &mut VarStore,
    addr: u32,
    ty: TypeId,
    frame: usize,
) -> Vec<Value> {
    let mut out = Vec::new();
    let Some(desc) = info.resolve_type(Some(ty)).and_then(|t| info.types.get(t)) else {
        return out;
    };
    let push = |out: &mut Vec<Value>, store: &mut VarStore, name: String, r: Rendered| {
        let mut v = json!({
            "name": name,
            "value": r.value,
            "type": r.ty,
            "variablesReference": r.children.map_or(0, |n| store.add(n)),
        });
        if let Some(addr) = r.memory {
            v["memoryReference"] = Value::from(format!("0x{addr:X}"));
        }
        out.push(v);
    };
    match desc {
        TypeDesc::Struct { members, .. } => {
            for m in members {
                let r = render(info, mem, m.ty, addr.wrapping_add(m.offset), frame);
                push(&mut out, store, m.name.clone(), r);
            }
        }
        TypeDesc::Array { element, count } => {
            let stride = info.type_size(*element).unwrap_or(1).max(1);
            let n = count.unwrap_or(0).min(256) as u32;
            for i in 0..n {
                let at = addr.wrapping_add((u64::from(i) * u64::from(stride)) as u32);
                let r = render(info, mem, *element, at, frame);
                push(&mut out, store, format!("[{i}]"), r);
            }
        }
        _ => {
            // A pointer target: one child, the pointee.
            let r = render(info, mem, Some(ty), addr, frame);
            push(&mut out, store, "*".into(), r);
        }
    }
    out
}

/// The variables of `frame`'s function visible at its PC.
pub fn locals(
    info: &DebugInfo,
    mem: &mut GuestMem,
    store: &mut VarStore,
    frame: &Frame,
    frame_index: usize,
) -> Vec<Value> {
    let Some(function) = info.function_at(lookup_pc(frame, frame_index)) else {
        return Vec::new();
    };
    let at = info.locate(lookup_pc(frame, frame_index));
    let visible = |v: &Variable| match (v.scope, at) {
        (None, _) => true,
        (Some((start, len)), Some(at)) => {
            at.hunk == start.hunk
                && at.offset >= start.offset
                && at.offset - start.offset < len.max(1)
        }
        _ => false,
    };
    let mut out = Vec::new();
    for v in function.params.iter().chain(function.locals.iter()) {
        if !visible(v) {
            continue;
        }
        let place = place_of(info, frame, Some(function), &v.location);
        out.push(variable_value(
            info,
            mem,
            store,
            &v.name,
            place,
            v.ty,
            frame,
            frame_index,
        ));
    }
    out
}

/// The address to look a frame's line/function up at: the PC itself
/// for the innermost frame, the call site (return address - 2) for the
/// callers, so a return address at the start of the next line maps to
/// the line of the call.
pub fn lookup_pc(frame: &Frame, index: usize) -> u32 {
    if index == 0 {
        frame.pc
    } else {
        frame.pc.wrapping_sub(2)
    }
}

/// The program's globals: DWARF variables, else data symbols as longs.
pub fn globals(
    info: &DebugInfo,
    mem: &mut GuestMem,
    store: &mut VarStore,
    frame: &Frame,
) -> Vec<Value> {
    let mut out = Vec::new();
    if !info.globals.is_empty() {
        for v in &info.globals {
            let place = place_of(info, frame, None, &v.location);
            out.push(variable_value(
                info, mem, store, &v.name, place, v.ty, frame, 0,
            ));
        }
        return out;
    }
    for sym in &info.symbols {
        let kind = info.hunks.get(sym.at.hunk as usize).map(|h| h.kind);
        if !matches!(
            kind,
            Some(crate::debuginfo::hunk::HunkKind::Data | crate::debuginfo::hunk::HunkKind::Bss)
        ) {
            continue;
        }
        if sym.name.starts_with("__") {
            continue;
        }
        let Some(addr) = info.runtime(sym.at) else {
            continue;
        };
        let value = mem.read32(addr).map_or("<unreadable>".into(), hex32);
        out.push(json!({
            "name": sym.name,
            "value": value,
            "type": "long",
            "evaluateName": sym.name,
            "variablesReference": 0,
            "memoryReference": format!("0x{addr:X}"),
        }));
    }
    out
}

/// A symbol's address, for the hover on a bare label.
pub fn symbol_address(info: &DebugInfo, name: &str) -> Option<HunkAddr> {
    let alt = match name.strip_prefix('_') {
        Some(rest) => rest.to_string(),
        None => format!("_{name}"),
    };
    info.symbols
        .iter()
        .find(|s| s.name == name || s.name == alt)
        .map(|s| s.at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_register_flags_decode() {
        assert_eq!(sr_flags(0x2704), "SI7--Z--");
        assert_eq!(sr_flags(0x0011), "I0X---C");
        assert_eq!(sr_flags(0x8000), "TI0-----");
    }

    #[test]
    fn var_store_references_are_one_based() {
        let mut store = VarStore::default();
        assert!(store.get(0).is_none());
        let r = store.add(Node::Globals);
        assert_eq!(r, 1);
        assert!(matches!(store.get(1), Some(Node::Globals)));
        store.clear();
        assert!(store.get(1).is_none());
    }
}
