// SPDX-License-Identifier: GPL-3.0-or-later

//! Breakpoint bookkeeping: the client's breakpoints by kind, each with
//! the DAP id it was given and the control-protocol break behind it
//! once installed. Source and function breakpoints resolve through the
//! program's debug information, so before the program is loaded they
//! wait unresolved and are installed at the load.

use super::eval;
use crate::control::bridge::{Bridge, Reply};
use crate::debugger::{BreakCond, CondOp, CondOperand};
use crate::debuginfo::DebugInfo;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// An exception filter offered on `initialize`.
pub struct ExceptionFilter {
    pub filter: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub vector: u16,
    pub default: bool,
}

pub const EXCEPTION_FILTERS: &[ExceptionFilter] = &[
    ExceptionFilter {
        filter: "bus-error",
        label: "Bus error",
        description: "Vector 2: access to an unmapped or unusable address.",
        vector: 2,
        default: false,
    },
    ExceptionFilter {
        filter: "address-error",
        label: "Address error",
        description: "SIGBUS (vector 3): a word or long access at an odd address.",
        vector: 3,
        default: true,
    },
    ExceptionFilter {
        filter: "illegal-instruction",
        label: "Illegal instruction",
        description: "SIGILL (vector 4): an illegal 68k instruction.",
        vector: 4,
        default: true,
    },
    ExceptionFilter {
        filter: "zero-divide",
        label: "Division by zero",
        description: "Vector 5: DIVU/DIVS by zero.",
        vector: 5,
        default: false,
    },
    ExceptionFilter {
        filter: "chk",
        label: "CHK / TRAPV",
        description: "Vectors 6 and 7: bound check and overflow traps.",
        vector: 6,
        default: false,
    },
    ExceptionFilter {
        filter: "privilege-violation",
        label: "Privilege violation",
        description: "Vector 8: a supervisor instruction in user mode.",
        vector: 8,
        default: false,
    },
    ExceptionFilter {
        filter: "line-a",
        label: "Line-A emulator",
        description: "Vector 10: an $Axxx opcode.",
        vector: 10,
        default: false,
    },
    ExceptionFilter {
        filter: "line-f",
        label: "Line-F emulator",
        description: "Vector 11: an $Fxxx opcode (coprocessor instruction with no FPU).",
        vector: 11,
        default: false,
    },
    ExceptionFilter {
        filter: "trap7",
        label: "TRAP #7",
        description: "SIGTRAP (vector 39): the conventional debugger break instruction.",
        vector: 39,
        default: true,
    },
];

/// One breakpoint the client set, whatever its kind.
#[derive(Debug, Clone)]
pub struct Breakpoint {
    pub id: i64,
    pub kind: Kind,
    /// The condition and hit count the client asked for, as the
    /// control protocol understands them.
    pub cond: Option<BreakCond>,
    pub ignore: u32,
    /// Why the breakpoint could not be installed, when it could not.
    pub message: Option<String>,
    /// Runtime address(es) it resolved to and the control-protocol ids.
    pub installed: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub enum Kind {
    Source {
        /// The client's path for the source, as sent.
        path: String,
        /// The line requested and the line it resolved to.
        line: u32,
        resolved_line: Option<u32>,
    },
    Function {
        name: String,
    },
    Instruction {
        addr: u32,
    },
    Data {
        addr: u32,
        bytes: u32,
        access: String,
    },
    Exception {
        vector: u16,
    },
}

impl Breakpoint {
    pub fn verified(&self) -> bool {
        !self.installed.is_empty()
    }

    /// The DAP `Breakpoint` object for a response or event.
    pub fn to_value(&self, lines_at_1: bool) -> Value {
        let mut v = json!({"id": self.id, "verified": self.verified()});
        if let Some(message) = &self.message {
            v["message"] = Value::from(message.clone());
        }
        match &self.kind {
            Kind::Source {
                path,
                line,
                resolved_line,
            } => {
                let line = resolved_line.unwrap_or(*line);
                v["line"] = json!(if lines_at_1 {
                    line
                } else {
                    line.saturating_sub(1)
                });
                v["source"] = json!({"path": path, "name": basename(path)});
            }
            Kind::Instruction { addr } => {
                v["instructionReference"] = Value::from(format!("0x{addr:X}"));
            }
            Kind::Data { access, .. } => {
                v["accessType"] = Value::from(match access.as_str() {
                    "access" => "readWrite",
                    other => other,
                });
            }
            Kind::Function { .. } | Kind::Exception { .. } => {}
        }
        if let Some((addr, _)) = self.installed.first() {
            if !matches!(self.kind, Kind::Data { .. } | Kind::Exception { .. }) {
                v["instructionReference"] = Value::from(format!("0x{addr:X}"));
            }
        }
        v
    }
}

pub fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Every breakpoint by DAP id, plus the exception catches by filter.
#[derive(Debug, Default)]
pub struct BreakTable {
    next_id: i64,
    pub points: BTreeMap<i64, Breakpoint>,
    /// `--run`'s own loadseg stop, or the one the session installed on
    /// attach, is not in `points`: control-protocol id when ours.
    pub loadseg_id: Option<u32>,
}

impl BreakTable {
    fn fresh_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// The DAP ids of the breakpoints installed at `pc`.
    pub fn ids_at(&self, pc: u32) -> Vec<i64> {
        self.points
            .values()
            .filter(|b| b.installed.iter().any(|(a, _)| *a == pc))
            .map(|b| b.id)
            .collect()
    }

    /// Remove every control-protocol break of the breakpoints `keep`
    /// rejects, and forget them.
    fn retire(&mut self, bridge: &Bridge, keep: impl Fn(&Breakpoint) -> bool) {
        let gone: Vec<i64> = self
            .points
            .values()
            .filter(|b| !keep(b))
            .map(|b| b.id)
            .collect();
        for id in gone {
            if let Some(bp) = self.points.remove(&id) {
                self.uninstall(bridge, &bp);
            }
        }
    }

    /// Install a PC breakpoint for `bp` at `addr`. An address another of
    /// our breakpoints already holds is shared when the conditions agree
    /// (recorded with id 0; the owner's removal hands the machine
    /// breakpoint over, see `uninstall`), refused when they differ; one
    /// the GUI owns is shared as-is, since its stop still reaches us.
    fn install_pc(&self, bridge: &Bridge, bp: &mut Breakpoint, addr: u32) {
        let mut params = json!({"kind": "pc", "addr": addr});
        if let Some(cond) = &bp.cond {
            params["cond"] = cond_value(cond);
        }
        if bp.ignore > 0 {
            params["ignore"] = json!(bp.ignore);
        }
        match bridge.call("break.add", params) {
            Ok(Reply::Ok(v)) => {
                if let Some(id) = v["id"].as_u64() {
                    bp.installed.push((addr, id as u32));
                }
            }
            Ok(Reply::Err { message, .. }) if message.contains("already set") => {
                let owner = self.points.values().find(|other| {
                    other.id != bp.id
                        && other.installed.iter().any(|(a, id)| *a == addr && *id != 0)
                });
                match owner {
                    Some(owner) if owner.cond != bp.cond || owner.ignore != bp.ignore => {
                        bp.message = Some(format!(
                            "0x{addr:X} already has a breakpoint with a different condition"
                        ));
                    }
                    _ => bp.installed.push((addr, 0)),
                }
            }
            Ok(Reply::Err { message, .. }) => bp.message = Some(message),
            Ok(Reply::TimedOut) | Err(_) => {}
        }
    }

    /// Remove `bp`'s machine breakpoints, except one another of our
    /// breakpoints shares: that one changes owner instead.
    fn uninstall(&mut self, bridge: &Bridge, bp: &Breakpoint) {
        for (addr, id) in &bp.installed {
            if *id == 0 {
                continue;
            }
            let sharer = self.points.values_mut().find(|other| {
                other.id != bp.id && other.installed.iter().any(|(a, i)| a == addr && *i == 0)
            });
            match sharer {
                Some(other) => {
                    for slot in other.installed.iter_mut().filter(|(a, _)| a == addr) {
                        slot.1 = *id;
                    }
                }
                None => {
                    let _ = bridge.call("break.remove", json!({"id": id}));
                }
            }
        }
    }

    /// `setBreakpoints`: replace the breakpoints of one source.
    pub fn set_source(
        &mut self,
        bridge: &Bridge,
        info: Option<&DebugInfo>,
        path: &str,
        requests: &[(u32, Option<String>, Option<String>)],
    ) -> Vec<i64> {
        self.retire(
            bridge,
            |b| !matches!(&b.kind, Kind::Source { path: p, .. } if p == path),
        );
        let mut ids = Vec::new();
        for (line, condition, hit) in requests {
            let (cond, ignore, message) = parse_conditions(condition.as_deref(), hit.as_deref());
            let mut bp = Breakpoint {
                id: self.fresh_id(),
                kind: Kind::Source {
                    path: path.to_string(),
                    line: *line,
                    resolved_line: None,
                },
                cond,
                ignore,
                message,
                installed: Vec::new(),
            };
            if bp.message.is_none() {
                self.resolve_and_install(bridge, info, &mut bp);
            }
            ids.push(bp.id);
            self.points.insert(bp.id, bp);
        }
        ids
    }

    /// `setFunctionBreakpoints`: replace them all.
    pub fn set_functions(
        &mut self,
        bridge: &Bridge,
        info: Option<&DebugInfo>,
        requests: &[(String, Option<String>, Option<String>)],
    ) -> Vec<i64> {
        self.retire(bridge, |b| !matches!(b.kind, Kind::Function { .. }));
        let mut ids = Vec::new();
        for (name, condition, hit) in requests {
            let (cond, ignore, message) = parse_conditions(condition.as_deref(), hit.as_deref());
            let mut bp = Breakpoint {
                id: self.fresh_id(),
                kind: Kind::Function { name: name.clone() },
                cond,
                ignore,
                message,
                installed: Vec::new(),
            };
            if bp.message.is_none() {
                self.resolve_and_install(bridge, info, &mut bp);
            }
            ids.push(bp.id);
            self.points.insert(bp.id, bp);
        }
        ids
    }

    /// `setInstructionBreakpoints`: replace them all.
    pub fn set_instructions(
        &mut self,
        bridge: &Bridge,
        requests: &[(u32, Option<String>, Option<String>)],
    ) -> Vec<i64> {
        self.retire(bridge, |b| !matches!(b.kind, Kind::Instruction { .. }));
        let mut ids = Vec::new();
        for (addr, condition, hit) in requests {
            let (cond, ignore, message) = parse_conditions(condition.as_deref(), hit.as_deref());
            let mut bp = Breakpoint {
                id: self.fresh_id(),
                kind: Kind::Instruction { addr: *addr },
                cond,
                ignore,
                message,
                installed: Vec::new(),
            };
            if bp.message.is_none() {
                self.install_pc(bridge, &mut bp, *addr);
            }
            ids.push(bp.id);
            self.points.insert(bp.id, bp);
        }
        ids
    }

    /// `setDataBreakpoints`: replace them all. Each covers the words of
    /// `[addr, addr + bytes)`, up to a cap like the GDB stub's.
    pub fn set_data(&mut self, bridge: &Bridge, requests: &[(u32, u32, String)]) -> Vec<i64> {
        self.retire(bridge, |b| !matches!(b.kind, Kind::Data { .. }));
        let mut ids = Vec::new();
        for (addr, bytes, access) in requests {
            let mut bp = Breakpoint {
                id: self.fresh_id(),
                kind: Kind::Data {
                    addr: *addr,
                    bytes: *bytes,
                    access: access.clone(),
                },
                cond: None,
                ignore: 0,
                message: None,
                installed: Vec::new(),
            };
            // Words from the aligned start through the last byte, so an
            // odd start still covers its final byte.
            let start = addr & !1;
            let end = addr.saturating_add((*bytes).max(1));
            let words = usize::try_from(end.saturating_sub(start).div_ceil(2)).unwrap_or(1);
            let capped = words.clamp(1, DATA_WORD_CAP);
            for i in 0..capped {
                let word = start.wrapping_add(i as u32 * 2);
                match bridge.call(
                    "break.add",
                    json!({"kind": "watch", "addr": word, "access": access}),
                ) {
                    Ok(Reply::Ok(v)) => {
                        if let Some(id) = v["id"].as_u64() {
                            bp.installed.push((word, id as u32));
                        }
                    }
                    Ok(Reply::Err { message, .. }) => {
                        bp.message = Some(message);
                        break;
                    }
                    Ok(Reply::TimedOut) | Err(_) => break,
                }
            }
            if words > capped {
                bp.message = Some(format!(
                    "watching the first {capped} words of {bytes} bytes"
                ));
            }
            ids.push(bp.id);
            self.points.insert(bp.id, bp);
        }
        ids
    }

    /// `setExceptionBreakpoints`: the filters that are on.
    pub fn set_exceptions(
        &mut self,
        bridge: &Bridge,
        filters: &[String],
        install: bool,
    ) -> Vec<i64> {
        self.retire(bridge, |b| !matches!(b.kind, Kind::Exception { .. }));
        let mut ids = Vec::new();
        for f in EXCEPTION_FILTERS
            .iter()
            .filter(|f| filters.iter().any(|x| x == f.filter))
        {
            // CHK and TRAPV share one filter.
            let vectors: &[u16] = if f.vector == 6 { &[6, 7] } else { &[f.vector] };
            let mut bp = Breakpoint {
                id: self.fresh_id(),
                kind: Kind::Exception { vector: f.vector },
                cond: None,
                ignore: 0,
                message: None,
                installed: Vec::new(),
            };
            if install {
                for &vector in vectors {
                    match bridge.call("break.add", json!({"kind": "catch", "vector": vector})) {
                        Ok(Reply::Ok(v)) => {
                            if let Some(id) = v["id"].as_u64() {
                                bp.installed.push((u32::from(vector), id as u32));
                            }
                        }
                        Ok(Reply::Err { message, .. }) => bp.message = Some(message),
                        _ => {}
                    }
                }
            }
            ids.push(bp.id);
            self.points.insert(bp.id, bp);
        }
        ids
    }

    /// Install exception points that were deliberately kept dormant while
    /// the operating system was loading the debug target.
    pub fn install_exceptions(&mut self, bridge: &Bridge) -> Vec<i64> {
        let ids: Vec<i64> = self
            .points
            .iter()
            .filter_map(|(id, bp)| {
                matches!(bp.kind, Kind::Exception { .. })
                    .then_some(*id)
                    .filter(|_| bp.installed.is_empty())
            })
            .collect();
        for id in &ids {
            let Some(bp) = self.points.get_mut(id) else {
                continue;
            };
            let Kind::Exception { vector } = bp.kind else {
                continue;
            };
            let vectors: &[u16] = if vector == 6 { &[6, 7] } else { &[vector] };
            for &vector in vectors {
                match bridge.call("break.add", json!({"kind": "catch", "vector": vector})) {
                    Ok(Reply::Ok(v)) => {
                        if let Some(control_id) = v["id"].as_u64() {
                            bp.installed.push((u32::from(vector), control_id as u32));
                        }
                    }
                    Ok(Reply::Err { message, .. }) => bp.message = Some(message),
                    _ => {}
                }
            }
        }
        ids
    }

    /// After the program loaded (or reloaded): resolve every source
    /// and function breakpoint against the relocated debug info.
    /// Returns the ids whose state changed.
    pub fn rebind(&mut self, bridge: &Bridge, info: &DebugInfo) -> Vec<i64> {
        let ids: Vec<i64> = self
            .points
            .values()
            .filter(|b| matches!(b.kind, Kind::Source { .. } | Kind::Function { .. }))
            .map(|b| b.id)
            .collect();
        let mut changed = Vec::new();
        for id in ids {
            let Some(mut bp) = self.points.remove(&id) else {
                continue;
            };
            let before = (bp.verified(), bp.installed.clone(), bp.message.clone());
            self.uninstall(bridge, &bp);
            bp.installed.clear();
            if let Some(message) = &bp.message {
                // A condition that never parsed stays unverified.
                if !message.starts_with("unresolved") {
                    self.points.insert(id, bp);
                    continue;
                }
            }
            bp.message = None;
            self.resolve_and_install(bridge, Some(info), &mut bp);
            if (bp.verified(), bp.installed.clone(), bp.message.clone()) != before {
                changed.push(id);
            }
            self.points.insert(id, bp);
        }
        changed
    }

    fn resolve_and_install(
        &mut self,
        bridge: &Bridge,
        info: Option<&DebugInfo>,
        bp: &mut Breakpoint,
    ) {
        let Some(info) = info.filter(|i| i.relocated()) else {
            bp.message = Some("unresolved: program not loaded yet".into());
            return;
        };
        match &mut bp.kind {
            Kind::Source {
                path,
                line,
                resolved_line,
            } => {
                let Some(file) = info.find_file(path) else {
                    bp.message = Some(format!(
                        "unresolved: no line information for {}",
                        basename(path)
                    ));
                    return;
                };
                let Some((actual, addrs)) = info.resolve_line(file, *line) else {
                    bp.message = Some(format!("unresolved: no code at or after line {line}"));
                    return;
                };
                *resolved_line = Some(actual);
                for addr in addrs {
                    self.install_pc(bridge, bp, addr);
                }
            }
            Kind::Function { name } => match info.lookup(name) {
                Some(addr) => self.install_pc(bridge, bp, addr),
                None => bp.message = Some(format!("unresolved: no symbol {name}")),
            },
            _ => {}
        }
    }

    /// Detach everything (session end).
    pub fn clear(&mut self, bridge: &Bridge) {
        let all: Vec<Breakpoint> = self.points.values().cloned().collect();
        self.points.clear();
        for bp in &all {
            for (_, id) in &bp.installed {
                if *id != 0 {
                    let _ = bridge.call("break.remove", json!({"id": id}));
                }
            }
        }
        if let Some(id) = self.loadseg_id.take() {
            let _ = bridge.call("break.remove", json!({"id": id}));
        }
    }
}

/// Word watches per data breakpoint, like the GDB stub's cap.
const DATA_WORD_CAP: usize = 8;

/// The control protocol's condition object.
fn cond_value(cond: &BreakCond) -> Value {
    let operand = |o: &CondOperand| match o {
        CondOperand::Data(n) => Value::from(format!("d{n}")),
        CondOperand::Addr(n) => Value::from(format!("a{n}")),
        CondOperand::Pc => Value::from("pc"),
        CondOperand::Sr => Value::from("sr"),
        CondOperand::Imm(v) => Value::from(*v),
        CondOperand::Mem(addr) => json!({"mem": addr}),
    };
    let op = match cond.op {
        CondOp::Eq => "eq",
        CondOp::Ne => "ne",
        CondOp::Lt => "lt",
        CondOp::Gt => "gt",
        CondOp::Le => "le",
        CondOp::Ge => "ge",
        CondOp::And => "and",
    };
    json!({"lhs": operand(&cond.lhs), "op": op, "rhs": operand(&cond.rhs)})
}

/// A client condition (`d0 == 5`, `[$DFF006].w & 1`, `a0 != 0`) and
/// hit condition (`5`, `== 5`, `>= 5` stop on the fifth hit, `> 5` on
/// the sixth) as the machine's own condition and ignore count. Anything
/// richer is refused with a message rather than silently ignored.
fn parse_conditions(
    condition: Option<&str>,
    hit: Option<&str>,
) -> (Option<BreakCond>, u32, Option<String>) {
    let mut message = None;
    let cond = match condition.map(str::trim).filter(|c| !c.is_empty()) {
        Some(text) => match eval::parse_condition(text) {
            Ok(cond) => Some(cond),
            Err(e) => {
                message = Some(format!("condition not supported: {e}"));
                None
            }
        },
        None => None,
    };
    let ignore = match hit.map(str::trim).filter(|h| !h.is_empty()) {
        Some(text) => {
            let (strict, digits) = if let Some(rest) = text.strip_prefix(">=") {
                (false, rest)
            } else if let Some(rest) = text.strip_prefix('>') {
                (true, rest)
            } else if let Some(rest) = text.strip_prefix("==") {
                (false, rest)
            } else if let Some(rest) = text.strip_prefix('=') {
                (false, rest)
            } else {
                (false, text)
            };
            match digits.trim().parse::<u32>() {
                Ok(n) if strict => n,
                Ok(n) => n.saturating_sub(1),
                Err(_) => {
                    message.get_or_insert(format!("hit condition {text} not supported"));
                    0
                }
            }
        }
        None => 0,
    };
    (cond, ignore, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_conditions_become_ignore_counts() {
        let (_, ignore, msg) = parse_conditions(None, Some("5"));
        assert_eq!((ignore, msg), (4, None));
        let (_, ignore, msg) = parse_conditions(None, Some(">= 3"));
        assert_eq!((ignore, msg), (2, None));
        let (_, ignore, msg) = parse_conditions(None, Some("> 3"));
        assert_eq!((ignore, msg), (3, None), "strictly greater: the fourth hit");
        let (_, ignore, msg) = parse_conditions(None, Some("== 2"));
        assert_eq!((ignore, msg), (1, None));
        let (_, ignore, msg) = parse_conditions(None, Some("% 2"));
        assert_eq!(ignore, 0);
        assert!(msg.unwrap().contains("not supported"));
    }

    #[test]
    fn conditions_parse_or_explain() {
        let (cond, _, msg) = parse_conditions(Some("d0 == 5"), None);
        assert!(cond.is_some());
        assert!(msg.is_none());
        let (cond, _, msg) = parse_conditions(Some("counter > d1 + 2"), None);
        assert!(cond.is_none());
        assert!(msg.unwrap().starts_with("condition not supported"));
    }

    #[test]
    fn breakpoint_values_carry_lines_and_verification() {
        let bp = Breakpoint {
            id: 3,
            kind: Kind::Source {
                path: "/src/main.c".into(),
                line: 10,
                resolved_line: Some(12),
            },
            cond: None,
            ignore: 0,
            message: None,
            installed: vec![(0x1234, 7)],
        };
        let v = bp.to_value(true);
        assert_eq!(v["verified"], true);
        assert_eq!(v["line"], 12);
        assert_eq!(v["source"]["name"], "main.c");
        assert_eq!(v["instructionReference"], "0x1234");
        assert_eq!(bp.to_value(false)["line"], 11);
    }
}
