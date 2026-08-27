// SPDX-License-Identifier: GPL-3.0-or-later

//! Disassemblers for the debugger: a 68000-family instruction disassembler
//! that resolves effective addresses and immediates by reading the operand
//! extension words from memory, and a Copper-list disassembler.
//!
//! The CPU disassembler covers the common integer instruction set used by
//! Amiga code (the full set of addressing modes, MOVE, the ALU groups,
//! branches, shifts/rotates, bit ops, MOVEM, and the miscellaneous 0x4xxx
//! opcodes). Anything it does not recognise is emitted as a `DC.W` word so a
//! trace never lies about an opcode it cannot decode. Multi-word operands are
//! resolved against memory through a caller-supplied word reader, so absolute
//! addresses, displacements, branch targets, and immediates print with their
//! real values rather than placeholders.

use m68k::CpuType;

const DN: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
const AN: [&str; 8] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];
const CC: [&str; 16] = [
    "T", "F", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT", "LE",
];

/// Sequential reader over the instruction stream. Tracks the absolute
/// address of each extension word so PC-relative operands resolve correctly.
struct Stream<'a> {
    read: &'a dyn Fn(u32) -> u16,
    base: u32,
    /// Number of words consumed so far (including the opcode word).
    words: u32,
    /// 68020+ decode: indexed extension words with bit 8 set are the full
    /// format (base/outer displacements, memory indirection). The 68000
    /// and 68010 ignore that bit and always use the brief format.
    full_ext: bool,
}

impl Stream<'_> {
    /// Address of the next extension word to be read.
    fn next_addr(&self) -> u32 {
        self.base.wrapping_add(self.words * 2)
    }

    fn next_word(&mut self) -> u16 {
        let w = (self.read)(self.next_addr());
        self.words += 1;
        w
    }

    fn next_long(&mut self) -> u32 {
        let hi = self.next_word() as u32;
        let lo = self.next_word() as u32;
        (hi << 16) | lo
    }
}

/// MOVEC control-register name for the extension word's Rc field, across
/// all models (010 through 060); unknown codes print as $NNN.
fn control_reg_name(code: u16) -> String {
    match code {
        0x000 => "SFC".into(),
        0x001 => "DFC".into(),
        0x002 => "CACR".into(),
        0x003 => "TC".into(),
        0x004 => "ITT0".into(),
        0x005 => "ITT1".into(),
        0x006 => "DTT0".into(),
        0x007 => "DTT1".into(),
        0x008 => "BUSCR".into(),
        0x800 => "USP".into(),
        0x801 => "VBR".into(),
        0x802 => "CAAR".into(),
        0x803 => "MSP".into(),
        0x804 => "ISP".into(),
        0x805 => "MMUSR".into(),
        0x806 => "URP".into(),
        0x807 => "SRP".into(),
        0x808 => "PCR".into(),
        other => format!("${other:03X}"),
    }
}

fn size_suffix(size: u8) -> &'static str {
    match size {
        0 => ".B",
        1 => ".W",
        2 => ".L",
        _ => "",
    }
}

fn signed_hex(v: i32) -> String {
    if v < 0 {
        format!("-${:X}", -(v as i64))
    } else {
        format!("${:X}", v)
    }
}

/// Decode a brief-format extension word index register, e.g. `D3.W*2`.
fn brief_index(ext: u16) -> String {
    let reg = ((ext >> 12) & 7) as usize;
    let is_addr = ext & 0x8000 != 0;
    let long = ext & 0x0800 != 0;
    let scale = (ext >> 9) & 3; // 020+; 0 (==*1) on 68000
    let name = if is_addr { AN[reg] } else { DN[reg] };
    let size = if long { "L" } else { "W" };
    if scale == 0 {
        format!("{name}.{size}")
    } else {
        format!("{name}.{size}*{}", 1 << scale)
    }
}

/// Render an indexed EA (`(d8,base,Xn)` brief format, or the 68020+ full
/// format when bit 8 of the extension word is set and the CPU honours it),
/// consuming any base/outer displacement words from `s`. `base` is "A0".."A7"
/// or "PC".
fn indexed_ea(base: &str, ext: u16, s: &mut Stream) -> String {
    if ext & 0x0100 == 0 || !s.full_ext {
        let d = ext as i8 as i32;
        return format!("({},{},{})", signed_hex(d), base, brief_index(ext));
    }
    // Full extension word format. Word order after it: base displacement,
    // then outer displacement (matching the execution model in the CPU
    // core's EA calculation).
    let base_suppress = ext & 0x0080 != 0;
    let index_suppress = ext & 0x0040 != 0;
    let bd = match (ext >> 4) & 3 {
        2 => Some(s.next_word() as i16 as i32),
        3 => Some(s.next_long() as i32),
        _ => None, // 0 reserved, 1 null displacement
    };
    let i_is = ext & 7;
    let od = if i_is != 0 {
        match i_is & 3 {
            2 => Some(s.next_word() as i16 as i32),
            3 => Some(s.next_long() as i32),
            _ => None, // null outer displacement
        }
    } else {
        None
    };
    let index = (!index_suppress).then(|| brief_index(ext));
    let mut inner: Vec<String> = Vec::new();
    if let Some(bd) = bd {
        inner.push(signed_hex(bd));
    }
    if !base_suppress {
        inner.push(base.to_string());
    }
    if i_is == 0 {
        // No memory indirection: (bd,base,Xn).
        if let Some(x) = index {
            inner.push(x);
        }
        if inner.is_empty() {
            inner.push("0".to_string());
        }
        return format!("({})", inner.join(","));
    }
    // Memory indirect: pre-indexed puts the index inside the brackets,
    // post-indexed applies it after the fetch.
    let post_indexed = i_is & 4 != 0;
    if !post_indexed {
        if let Some(x) = index.clone() {
            inner.push(x);
        }
    }
    if inner.is_empty() {
        inner.push("0".to_string());
    }
    let mut outer = format!("[{}]", inner.join(","));
    if post_indexed {
        if let Some(x) = index {
            outer = format!("{outer},{x}");
        }
    }
    if let Some(od) = od {
        outer = format!("{outer},{}", signed_hex(od));
    }
    format!("({outer})")
}

/// Decode the effective address with mode/reg fields and an operand size
/// (0=byte, 1=word, 2=long), consuming any extension words from `s`.
fn effective_address(mode: u8, reg: u8, size: u8, s: &mut Stream) -> String {
    match mode {
        0 => DN[reg as usize].to_string(),
        1 => AN[reg as usize].to_string(),
        2 => format!("({})", AN[reg as usize]),
        3 => format!("({})+", AN[reg as usize]),
        4 => format!("-({})", AN[reg as usize]),
        5 => {
            let d = s.next_word() as i16 as i32;
            format!("({},{})", signed_hex(d), AN[reg as usize])
        }
        6 => {
            let ext = s.next_word();
            indexed_ea(AN[reg as usize], ext, s)
        }
        7 => match reg {
            0 => {
                let a = s.next_word() as i16 as i32;
                format!("(${:X}).W", a as u32 & 0xFFFF)
            }
            1 => {
                let a = s.next_long();
                format!("(${:X}).L", a)
            }
            2 => {
                let at = s.next_addr();
                let d = s.next_word() as i16 as i32;
                format!("({},PC)", signed_hex(d)) + &format!(" ; ${:X}", at.wrapping_add(d as u32))
            }
            3 => {
                let ext = s.next_word();
                indexed_ea("PC", ext, s)
            }
            4 => match size {
                0 => format!("#${:X}", s.next_word() & 0xFF),
                1 => format!("#${:X}", s.next_word()),
                _ => format!("#${:X}", s.next_long()),
            },
            _ => "<?>".to_string(),
        },
        _ => "<?>".to_string(),
    }
}

/// Disassemble one instruction at `pc`, reading opcode and operand words via
/// `read`. Returns the formatted text and the instruction length in bytes.
pub fn disassemble(read: impl Fn(u32) -> u16, pc: u32, cpu_type: CpuType) -> (String, u32) {
    let mut s = Stream {
        read: &read,
        base: pc,
        words: 0,
        full_ext: !matches!(cpu_type, CpuType::M68000 | CpuType::M68010),
    };
    let op = s.next_word();
    let text = decode(op, &mut s, cpu_type);
    let text = text.unwrap_or_else(|| {
        // Reset to a single-word DC.W for anything unrecognised.
        format!("DC.W ${op:04X}")
    });
    // If we fell back to DC.W, the length is one word; otherwise it is the
    // number of words the decoder consumed.
    let words = if text.starts_with("DC.W ") {
        1
    } else {
        s.words
    };
    (text, words * 2)
}

fn decode(op: u16, s: &mut Stream, _cpu_type: CpuType) -> Option<String> {
    match op >> 12 {
        0x0 => decode_0(op, s),
        0x1 => decode_move(op, 0, s),
        0x2 => decode_move(op, 2, s),
        0x3 => decode_move(op, 1, s),
        0x4 => decode_4(op, s),
        0x5 => decode_5(op, s),
        0x6 => Some(decode_branch(op, s)),
        0x7 => {
            if op & 0x0100 != 0 {
                return None;
            }
            let d = ((op >> 9) & 7) as usize;
            let data = (op & 0xFF) as i8 as i32;
            Some(format!("MOVEQ #{},{}", signed_hex(data), DN[d]))
        }
        0x8 => decode_or_div_sbcd(op, s),
        0x9 => decode_addsub(op, "SUB", s),
        0xB => decode_b(op, s),
        0xC => decode_and_mul_abcd_exg(op, s),
        0xD => decode_addsub(op, "ADD", s),
        0xE => decode_shift(op, s),
        _ => None,
    }
}

fn decode_move(op: u16, size: u8, s: &mut Stream) -> Option<String> {
    let src_mode = ((op >> 3) & 7) as u8;
    let src_reg = (op & 7) as u8;
    let dst_mode = ((op >> 6) & 7) as u8;
    let dst_reg = ((op >> 9) & 7) as u8;
    let src = effective_address(src_mode, src_reg, size, s);
    let dst = effective_address(dst_mode, dst_reg, size, s);
    let mnem = if dst_mode == 1 { "MOVEA" } else { "MOVE" };
    Some(format!("{mnem}{} {src},{dst}", size_suffix(size)))
}

fn decode_0(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let size = ((op >> 6) & 3) as u8;
    // Immediate ALU ops: ORI/ANDI/SUBI/ADDI/EORI/CMPI and the special
    // ANDI/ORI/EORI to CCR/SR encodings.
    let imm_mnem = match (op >> 9) & 7 {
        0 => Some("ORI"),
        1 => Some("ANDI"),
        2 => Some("SUBI"),
        3 => Some("ADDI"),
        5 => Some("EORI"),
        6 => Some("CMPI"),
        _ => None,
    };
    if op & 0x0100 == 0 {
        if let Some(mnem) = imm_mnem {
            if size != 3 {
                // ANDI/ORI/EORI #imm,CCR or ,SR
                if (op & 0x00FF) == 0x003C || (op & 0x00FF) == 0x007C {
                    let to_sr = (op & 0x0040) != 0;
                    let imm = s.next_word();
                    let dst = if to_sr { "SR" } else { "CCR" };
                    return Some(format!("{mnem} #${imm:X},{dst}"));
                }
                let imm = match size {
                    0 => format!("#${:X}", s.next_word() & 0xFF),
                    1 => format!("#${:X}", s.next_word()),
                    _ => format!("#${:X}", s.next_long()),
                };
                let ea = effective_address(mode, reg, size, s);
                return Some(format!("{mnem}{} {imm},{ea}", size_suffix(size)));
            }
        }
    }
    // Static bit ops: BTST/BCHG/BCLR/BSET #imm,<ea>  (op bits 11-8 = 1000+)
    if (op & 0x0F00) >> 8 == 0x8 {
        let bit_mnem = ["BTST", "BCHG", "BCLR", "BSET"][((op >> 6) & 3) as usize];
        let imm = s.next_word() & 0xFF;
        let ea = effective_address(mode, reg, 0, s);
        return Some(format!("{bit_mnem} #{imm},{ea}"));
    }
    // Bit 8 set: either MOVEP (mode field == 001) or a dynamic bit op
    // (BTST/BCHG/BCLR/BSET Dn,<ea>).
    if op & 0x0100 != 0 {
        // MOVEP: bit 8 set, mode field == 001
        if mode == 1 {
            let dn = ((op >> 9) & 7) as usize;
            let dir_to_mem = op & 0x0080 != 0;
            let sz = if op & 0x0040 != 0 { ".L" } else { ".W" };
            let d = s.next_word() as i16 as i32;
            let mem = format!("({},{})", signed_hex(d), AN[reg as usize]);
            return Some(if dir_to_mem {
                format!("MOVEP{sz} {},{mem}", DN[dn])
            } else {
                format!("MOVEP{sz} {mem},{}", DN[dn])
            });
        }
        let bit_mnem = ["BTST", "BCHG", "BCLR", "BSET"][((op >> 6) & 3) as usize];
        let dn = ((op >> 9) & 7) as usize;
        let ea = effective_address(mode, reg, 0, s);
        return Some(format!("{bit_mnem} {},{ea}", DN[dn]));
    }
    None
}

fn decode_branch(op: u16, s: &mut Stream) -> String {
    let cc = ((op >> 8) & 0xF) as usize;
    let at = pc_after_opcode(s);
    let disp8 = (op & 0xFF) as i8 as i32;
    let (disp, suffix) = if (op & 0xFF) == 0x00 {
        (s.next_word() as i16 as i32, ".W")
    } else if (op & 0xFF) == 0xFF {
        (s.next_long() as i32, ".L")
    } else {
        (disp8, ".B")
    };
    let target = at.wrapping_add(disp as u32);
    let mnem = match cc {
        0 => "BRA".to_string(),
        1 => "BSR".to_string(),
        _ => format!("B{}", CC[cc]),
    };
    format!("{mnem}{suffix} ${target:X}")
}

/// Address of the word immediately after the opcode word (the reference point
/// for byte/word branch displacements).
fn pc_after_opcode(s: &Stream) -> u32 {
    s.base.wrapping_add(2)
}

fn decode_5(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let size = ((op >> 6) & 3) as u8;
    if size == 3 {
        let cc = ((op >> 8) & 0xF) as usize;
        if mode == 1 {
            // DBcc Dn,disp
            let at = s.next_addr();
            let d = s.next_word() as i16 as i32;
            let target = at.wrapping_add(d as u32);
            return Some(format!("DB{} {},${target:X}", CC[cc], DN[reg as usize]));
        }
        // Scc <ea>
        let ea = effective_address(mode, reg, 0, s);
        return Some(format!("S{} {ea}", CC[cc]));
    }
    // ADDQ/SUBQ #data,<ea>
    let mut data = ((op >> 9) & 7) as u32;
    if data == 0 {
        data = 8;
    }
    let mnem = if op & 0x0100 != 0 { "SUBQ" } else { "ADDQ" };
    let ea = effective_address(mode, reg, size, s);
    Some(format!("{mnem}{} #{data},{ea}", size_suffix(size)))
}

fn decode_4(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let size = ((op >> 6) & 3) as u8;

    // Fixed single-word opcodes.
    match op {
        0x4E70 => return Some("RESET".into()),
        0x4E71 => return Some("NOP".into()),
        0x4E72 => {
            let imm = s.next_word();
            return Some(format!("STOP #${imm:X}"));
        }
        0x4E73 => return Some("RTE".into()),
        0x4E75 => return Some("RTS".into()),
        0x4E76 => return Some("TRAPV".into()),
        0x4E77 => return Some("RTR".into()),
        0x4AFC => return Some("ILLEGAL".into()),
        0x4E7A | 0x4E7B => {
            let ext = s.next_word();
            let rn = if ext & 0x8000 != 0 {
                AN[((ext >> 12) & 7) as usize]
            } else {
                DN[((ext >> 12) & 7) as usize]
            };
            let rc = control_reg_name(ext & 0xFFF);
            return Some(if op == 0x4E7A {
                format!("MOVEC {rc},{rn}")
            } else {
                format!("MOVEC {rn},{rc}")
            });
        }
        _ => {}
    }
    match op & 0xFFF8 {
        0x4E50 => {
            let d = s.next_word() as i16 as i32;
            return Some(format!("LINK {},#{}", AN[reg as usize], signed_hex(d)));
        }
        0x4E58 => return Some(format!("UNLK {}", AN[reg as usize])),
        0x4E60 => return Some(format!("MOVE {},USP", AN[reg as usize])),
        0x4E68 => return Some(format!("MOVE USP,{}", AN[reg as usize])),
        0x4840 => return Some(format!("SWAP {}", DN[reg as usize])),
        0x4880 => return Some(format!("EXT.W {}", DN[reg as usize])),
        0x48C0 => return Some(format!("EXT.L {}", DN[reg as usize])),
        _ => {}
    }
    if op & 0xFFF0 == 0x4E40 {
        return Some(format!("TRAP #{}", op & 0xF));
    }

    // Operations keyed off bits 11-8.
    match (op >> 8) & 0xF {
        0x0 if size != 3 => {
            let ea = effective_address(mode, reg, size, s);
            return Some(format!("NEGX{} {ea}", size_suffix(size)));
        }
        0x2 if size != 3 => {
            let ea = effective_address(mode, reg, size, s);
            return Some(format!("CLR{} {ea}", size_suffix(size)));
        }
        0x4 if size != 3 => {
            let ea = effective_address(mode, reg, size, s);
            return Some(format!("NEG{} {ea}", size_suffix(size)));
        }
        0x6 if size != 3 => {
            let ea = effective_address(mode, reg, size, s);
            return Some(format!("NOT{} {ea}", size_suffix(size)));
        }
        0xA if size != 3 => {
            let ea = effective_address(mode, reg, size, s);
            return Some(format!("TST{} {ea}", size_suffix(size)));
        }
        _ => {}
    }
    // MOVE to/from CCR/SR
    match op & 0xFFC0 {
        0x44C0 => {
            let ea = effective_address(mode, reg, 1, s);
            return Some(format!("MOVE {ea},CCR"));
        }
        0x46C0 => {
            let ea = effective_address(mode, reg, 1, s);
            return Some(format!("MOVE {ea},SR"));
        }
        0x40C0 => {
            let ea = effective_address(mode, reg, 1, s);
            return Some(format!("MOVE SR,{ea}"));
        }
        0x4840 => {
            let ea = effective_address(mode, reg, 2, s);
            return Some(format!("PEA {ea}"));
        }
        0x4E80 => {
            let ea = effective_address(mode, reg, 2, s);
            return Some(format!("JSR {ea}"));
        }
        0x4EC0 => {
            let ea = effective_address(mode, reg, 2, s);
            return Some(format!("JMP {ea}"));
        }
        _ => {}
    }
    // LEA An,<ea>
    if op & 0xF1C0 == 0x41C0 {
        let an = ((op >> 9) & 7) as usize;
        let ea = effective_address(mode, reg, 2, s);
        return Some(format!("LEA {ea},{}", AN[an]));
    }
    // CHK <ea>,Dn
    if op & 0xF1C0 == 0x4180 {
        let dn = ((op >> 9) & 7) as usize;
        let ea = effective_address(mode, reg, 1, s);
        return Some(format!("CHK {ea},{}", DN[dn]));
    }
    // MOVEM <list>,<ea> / <ea>,<list>
    if op & 0xFB80 == 0x4880 {
        let to_mem = op & 0x0400 == 0;
        let long = op & 0x0040 != 0;
        let mask = s.next_word();
        let predec = mode == 4;
        let list = movem_list(mask, predec);
        let ea = effective_address(mode, reg, if long { 2 } else { 1 }, s);
        let sz = if long { ".L" } else { ".W" };
        return Some(if to_mem {
            format!("MOVEM{sz} {list},{ea}")
        } else {
            format!("MOVEM{sz} {ea},{list}")
        });
    }
    None
}

fn movem_list(mask: u16, predec: bool) -> String {
    // Bit order: A7..A0,D7..D0 for predecrement; D0..D7,A0..A7 otherwise.
    let mut names = Vec::new();
    for i in 0..16 {
        let set = mask & (1 << i) != 0;
        if !set {
            continue;
        }
        let idx = if predec { 15 - i } else { i };
        if idx < 8 {
            names.push(DN[idx]);
        } else {
            names.push(AN[idx - 8]);
        }
    }
    if names.is_empty() {
        "0".into()
    } else {
        names.join("/")
    }
}

fn dual_reg_operands(mode: u8, reg: u8, dn: usize) -> (String, String) {
    if mode == 1 {
        (format!("-({})", AN[reg as usize]), format!("-({})", AN[dn]))
    } else {
        (DN[reg as usize].to_string(), DN[dn].to_string())
    }
}

fn format_dir_op(mnem: &str, size: u8, dn: usize, ea: &str, dir_to_ea: bool) -> String {
    if dir_to_ea {
        format!("{mnem}{} {},{ea}", size_suffix(size), DN[dn])
    } else {
        format!("{mnem}{} {ea},{}", size_suffix(size), DN[dn])
    }
}

fn decode_addsub(op: u16, base: &str, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let dn = ((op >> 9) & 7) as usize;
    let opmode = ((op >> 6) & 7) as u8;
    if opmode == 3 || opmode == 7 {
        let size = if opmode == 7 { 2 } else { 1 };
        let ea = effective_address(mode, reg, size, s);
        return Some(format!("{base}A{} {ea},{}", size_suffix(size), AN[dn]));
    }
    let size = opmode & 3;
    if opmode & 4 != 0 && (mode == 0 || mode == 1) {
        let (x, y) = dual_reg_operands(mode, reg, dn);
        return Some(format!("{base}X{} {x},{y}", size_suffix(size)));
    }
    let ea = effective_address(mode, reg, size, s);
    Some(format_dir_op(base, size, dn, &ea, opmode & 4 != 0))
}

fn decode_or_div_sbcd(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let dn = ((op >> 9) & 7) as usize;
    let opmode = ((op >> 6) & 7) as u8;
    if opmode == 3 || opmode == 7 {
        let mnem = if opmode == 7 { "DIVS" } else { "DIVU" };
        let ea = effective_address(mode, reg, 1, s);
        return Some(format!("{mnem} {ea},{}", DN[dn]));
    }
    if opmode == 4 && (mode == 0 || mode == 1) {
        let (x, y) = dual_reg_operands(mode, reg, dn);
        return Some(format!("SBCD {x},{y}"));
    }
    let size = opmode & 3;
    let ea = effective_address(mode, reg, size, s);
    Some(format_dir_op("OR", size, dn, &ea, opmode & 4 != 0))
}

fn decode_and_mul_abcd_exg(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let dn = ((op >> 9) & 7) as usize;
    let opmode = ((op >> 6) & 7) as u8;
    if opmode == 3 || opmode == 7 {
        let mnem = if opmode == 7 { "MULS" } else { "MULU" };
        let ea = effective_address(mode, reg, 1, s);
        return Some(format!("{mnem} {ea},{}", DN[dn]));
    }
    if opmode == 4 && (mode == 0 || mode == 1) {
        let (x, y) = dual_reg_operands(mode, reg, dn);
        return Some(format!("ABCD {x},{y}"));
    }
    if opmode == 5 && mode == 0 {
        return Some(format!("EXG {},{}", DN[dn], DN[reg as usize]));
    }
    if opmode == 5 && mode == 1 {
        return Some(format!("EXG {},{}", AN[dn], AN[reg as usize]));
    }
    if opmode == 6 && mode == 1 {
        return Some(format!("EXG {},{}", DN[dn], AN[reg as usize]));
    }
    let size = opmode & 3;
    let ea = effective_address(mode, reg, size, s);
    Some(format_dir_op("AND", size, dn, &ea, opmode & 4 != 0))
}

fn decode_b(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let dn = ((op >> 9) & 7) as usize;
    let opmode = ((op >> 6) & 7) as u8;
    // CMPA
    if opmode == 3 || opmode == 7 {
        let size = if opmode == 7 { 2 } else { 1 };
        let ea = effective_address(mode, reg, size, s);
        return Some(format!("CMPA{} {ea},{}", size_suffix(size), AN[dn]));
    }
    let size = opmode & 3;
    if opmode & 4 != 0 {
        // CMPM (An)+,(An)+ when mode==1, else EOR Dn,<ea>
        if mode == 1 {
            return Some(format!(
                "CMPM{} ({})+,({})+",
                size_suffix(size),
                AN[reg as usize],
                AN[dn]
            ));
        }
        let ea = effective_address(mode, reg, size, s);
        return Some(format!("EOR{} {},{ea}", size_suffix(size), DN[dn]));
    }
    // CMP <ea>,Dn
    let ea = effective_address(mode, reg, size, s);
    Some(format!("CMP{} {ea},{}", size_suffix(size), DN[dn]))
}

fn decode_shift(op: u16, s: &mut Stream) -> Option<String> {
    let mode = ((op >> 3) & 7) as u8;
    let reg = (op & 7) as u8;
    let size = ((op >> 6) & 3) as u8;
    let names = ["AS", "LS", "ROX", "RO"];
    if size == 3 {
        // Memory shift by one: <ea>
        let kind = ((op >> 9) & 3) as usize;
        let dir = if op & 0x0100 != 0 { "L" } else { "R" };
        let ea = effective_address(mode, reg, 1, s);
        return Some(format!("{}{dir} {ea}", names[kind]));
    }
    let kind = (op & 0x18) >> 3;
    let dir = if op & 0x0100 != 0 { "L" } else { "R" };
    let count_or_reg = ((op >> 9) & 7) as usize;
    let ir = op & 0x0020 != 0; // count in register
    let src = if ir {
        DN[count_or_reg].to_string()
    } else {
        let c = if count_or_reg == 0 { 8 } else { count_or_reg };
        format!("#{c}")
    };
    Some(format!(
        "{}{dir}{} {src},{}",
        names[kind as usize],
        size_suffix(size),
        DN[reg as usize]
    ))
}

// ---------------------------------------------------------------------------
// Copper
// ---------------------------------------------------------------------------

/// Disassemble one Copper instruction from its two 16-bit words (IR1, IR2).
///
/// The Copper has exactly three instruction forms:
/// - MOVE  #data,$dff0xx   (IR1 bit 0 == 0): write a custom register.
/// - WAIT  vp,hp[,mask]    (IR1 bit 0 == 1, IR2 bit 0 == 0): wait for the beam.
/// - SKIP  vp,hp[,mask]    (IR1 bit 0 == 1, IR2 bit 0 == 1): skip the next MOVE
///   if the beam is at/after the position.
pub fn disassemble_copper(ir1: u16, ir2: u16) -> String {
    if ir1 & 1 == 0 {
        // MOVE: register offset is IR1 bits 8..1 (DFF000 + (IR1 & 0x1FE)).
        let reg = ir1 & 0x01FE;
        return format!("MOVE  #${ir2:04X},$DFF{reg:03X}");
    }
    let vp = (ir1 >> 8) & 0xFF;
    let hp = ir1 & 0x00FE;
    let ve = (ir2 >> 8) & 0x7F;
    let he = ir2 & 0x00FE;
    let bfd = ir2 & 0x8000 != 0;
    let kind = if ir2 & 1 == 0 { "WAIT" } else { "SKIP" };
    // The decimal beam position matches the coordinates the debugger's
    // Chipset tab, beam traps, and Frame Analyzer display.
    let mut out = format!("{kind}  vp=${vp:02X},hp=${hp:02X} (v{vp} h{hp})");
    // Show the comparison mask only when it is not the all-ones default, and
    // note blitter-finished-disable for WAIT/SKIP.
    if ve != 0x7F || he != 0xFE {
        out.push_str(&format!(" (mask vp=${ve:02X},hp=${he:02X})"));
    }
    if !bfd {
        out.push_str(" [BFD]");
    }
    out
}

/// Disassemble a Copper list starting at `start`, reading words via `read`,
/// up to `max` instructions. Stops early at the end-of-list WAIT
/// ($FFFF,$FFFE). Returns `(address, text)` per instruction.
pub fn dump_copper_list(read: impl Fn(u32) -> u16, start: u32, max: usize) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut addr = start & !1;
    for _ in 0..max {
        let ir1 = read(addr);
        let ir2 = read(addr.wrapping_add(2));
        out.push((addr, disassemble_copper(ir1, ir2)));
        addr = addr.wrapping_add(4);
        if ir1 == 0xFFFF && ir2 == 0xFFFE {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disassemble a slice of words placed at `pc`.
    fn dis(words: &[u16], pc: u32) -> (String, u32) {
        let mem = words.to_vec();
        disassemble(
            move |addr| {
                let idx = (addr.wrapping_sub(pc) / 2) as usize;
                mem.get(idx).copied().unwrap_or(0)
            },
            pc,
            CpuType::M68000,
        )
    }

    #[test]
    fn simple_fixed_opcodes() {
        assert_eq!(dis(&[0x4E71], 0).0, "NOP");
        assert_eq!(dis(&[0x4E75], 0).0, "RTS");
        assert_eq!(dis(&[0x4E73], 0).0, "RTE");
        assert_eq!(dis(&[0x4E77], 0).0, "RTR");
    }

    #[test]
    fn movec_names_registers_both_directions() {
        // MOVEC A0,VBR - the 680x0.library probe-handler idiom.
        assert_eq!(dis(&[0x4E7B, 0x8801], 0), ("MOVEC A0,VBR".into(), 4));
        // MOVEC PCR,D0 - the 060 probe an 040 must trap on.
        assert_eq!(dis(&[0x4E7A, 0x0808], 0).0, "MOVEC PCR,D0");
        assert_eq!(dis(&[0x4E7B, 0x1002], 0).0, "MOVEC D1,CACR");
        // Unknown control codes print raw.
        assert_eq!(dis(&[0x4E7A, 0x0123], 0).0, "MOVEC $123,D0");
    }

    #[test]
    fn moveq_and_move() {
        assert_eq!(dis(&[0x7001], 0), ("MOVEQ #$1,D0".into(), 2));
        assert_eq!(dis(&[0x7280], 0).0, "MOVEQ #-$80,D1");
        assert_eq!(dis(&[0x2200], 0), ("MOVE.L D0,D1".into(), 2));
        // MOVE.W (A0),D3
        assert_eq!(dis(&[0x3610], 0).0, "MOVE.W (A0),D3");
    }

    #[test]
    fn move_immediate_and_absolute() {
        // MOVE.L #$12345678,$00C00000  (immediate then abs.L destination)
        let (t, n) = dis(&[0x23FC, 0x1234, 0x5678, 0x00C0, 0x0000], 0);
        assert_eq!(t, "MOVE.L #$12345678,($C00000).L");
        assert_eq!(n, 10);
    }

    #[test]
    fn displacement_addressing() {
        // MOVE.W $4(A0),D0 -> 3028 0004
        assert_eq!(dis(&[0x3028, 0x0004], 0).0, "MOVE.W ($4,A0),D0");
    }

    /// Disassemble as a 68020, where indexed extension words with bit 8 set
    /// use the full format (base/outer displacements, memory indirection).
    fn dis020(words: &[u16], pc: u32) -> (String, u32) {
        let mem = words.to_vec();
        disassemble(
            move |addr| {
                let idx = (addr.wrapping_sub(pc) / 2) as usize;
                mem.get(idx).copied().unwrap_or(0)
            },
            pc,
            CpuType::M68EC020,
        )
    }

    #[test]
    fn full_extension_memory_indirect() {
        // MOVE.W ([$25DE,A4],$C),D0: index suppressed, word base
        // displacement, memory indirect with word outer displacement.
        let (t, n) = dis020(&[0x3034, 0x0162, 0x25DE, 0x000C], 0);
        assert_eq!(t, "MOVE.W ([$25DE,A4],$C),D0");
        assert_eq!(n, 8);
        // MOVEA.L ([$25DE,A4],$10),A0: the vectored-call form of the same EA.
        let (t, n) = dis020(&[0x2074, 0x0162, 0x25DE, 0x0010], 0);
        assert_eq!(t, "MOVEA.L ([$25DE,A4],$10),A0");
        assert_eq!(n, 8);
    }

    #[test]
    fn full_extension_pre_indexed_and_plain() {
        // Pre-indexed with a live index register and null outer displacement.
        let (t, n) = dis020(&[0x3034, 0x0121, 0x0040], 0);
        assert_eq!(t, "MOVE.W ([$40,A4,D0.W]),D0");
        assert_eq!(n, 6);
        // No memory indirection, long base displacement: (bd32,An,Xn).
        let (t, n) = dis020(&[0x3034, 0x0130, 0x0001, 0x2345], 0);
        assert_eq!(t, "MOVE.W ($12345,A4,D0.W),D0");
        assert_eq!(n, 8);
    }

    #[test]
    fn full_extension_bit_ignored_on_68000() {
        // The 68000 has no full-format extension words: bit 8 is ignored and
        // the word decodes as the brief format (d8 = low byte).
        let (t, n) = dis(&[0x3034, 0x0162, 0x25DE, 0x000C], 0);
        assert_eq!(t, "MOVE.W ($62,A4,D0.W),D0");
        assert_eq!(n, 4);
    }

    #[test]
    fn branches_resolve_targets() {
        // BRA.B to pc+2+4 = 6 ; opcode 6004 at pc 0
        assert_eq!(dis(&[0x6004], 0).0, "BRA.B $6");
        // BNE.W with word displacement
        let (t, n) = dis(&[0x6600, 0x0010], 0x1000);
        assert_eq!(t, "BNE.W $1012");
        assert_eq!(n, 4);
        // BSR.B
        assert_eq!(dis(&[0x6102], 0x2000).0, "BSR.B $2004");
    }

    #[test]
    fn alu_and_immediate_groups() {
        // ADD.W D1,D0 -> D041
        assert_eq!(dis(&[0xD041], 0).0, "ADD.W D1,D0");
        // ADDI.W #$10,D0 -> 0640 0010
        assert_eq!(dis(&[0x0640, 0x0010], 0).0, "ADDI.W #$10,D0");
        // CMP.L A0... use CMP.W (A0),D0 -> B050
        assert_eq!(dis(&[0xB050], 0).0, "CMP.W (A0),D0");
        // LEA $2(A0),A1 -> 43E8 0002
        assert_eq!(dis(&[0x43E8, 0x0002], 0).0, "LEA ($2,A0),A1");
        // JSR (A0) -> 4E90
        assert_eq!(dis(&[0x4E90], 0).0, "JSR (A0)");
    }

    #[test]
    fn movem_lists() {
        // MOVEM.L D0-D1/A0,-(A7): predecrement, push order.
        // mask for predec where bit0=A7..bit15=D0; D0,D1,A0 set ->
        // names D0/D1/A0. opcode 48E7 then mask.
        let mask = 0b1100_0000_1000_0000u16; // bits 15,14 (D0,D1) and 8 (A0) in predec order
        let (t, _) = dis(&[0x48E7, mask], 0);
        assert!(t.starts_with("MOVEM.L "), "{t}");
        assert!(t.ends_with(",-(A7)"), "{t}");
    }

    #[test]
    fn shifts() {
        // LSL.L #1,D0 -> E388 ; kind LS (1), dir L, size .L, count 1
        assert_eq!(dis(&[0xE388], 0).0, "LSL.L #1,D0");
        // ASR.W D2,D3 -> shift by reg
        assert_eq!(dis(&[0xE423], 0).0, "ASR.B D2,D3");
    }

    #[test]
    fn alu_directions_and_dual_reg() {
        assert_eq!(dis(&[0xD050], 0), ("ADD.W (A0),D0".into(), 2));
        assert_eq!(dis(&[0xD150], 0), ("ADD.W D0,(A0)".into(), 2));
        assert_eq!(dis(&[0xD2D0], 0), ("ADDA.W (A0),A1".into(), 2));
        assert_eq!(dis(&[0xD3D0], 0), ("ADDA.L (A0),A1".into(), 2));
        assert_eq!(dis(&[0x9150], 0), ("SUB.W D0,(A0)".into(), 2));
        assert_eq!(dis(&[0x9390], 0), ("SUB.L D1,(A0)".into(), 2));
        assert_eq!(dis(&[0xD101], 0), ("ADDX.B D1,D0".into(), 2));
        assert_eq!(dis(&[0xD149], 0), ("ADDX.W -(A1),-(A0)".into(), 2));
        assert_eq!(dis(&[0xD38A], 0), ("ADDX.L -(A2),-(A1)".into(), 2));
        assert_eq!(dis(&[0x9342], 0), ("SUBX.W D2,D1".into(), 2));
        assert_eq!(dis(&[0x9189], 0), ("SUBX.L -(A1),-(A0)".into(), 2));
        assert_eq!(dis(&[0x8010], 0), ("OR.B (A0),D0".into(), 2));
        assert_eq!(dis(&[0x8150], 0), ("OR.W D0,(A0)".into(), 2));
        assert_eq!(dis(&[0x80D0], 0), ("DIVU (A0),D0".into(), 2));
        assert_eq!(dis(&[0x81D0], 0), ("DIVS (A0),D0".into(), 2));
        assert_eq!(dis(&[0x8101], 0), ("SBCD D1,D0".into(), 2));
        assert_eq!(dis(&[0x8109], 0), ("SBCD -(A1),-(A0)".into(), 2));
        assert_eq!(dis(&[0xC010], 0), ("AND.B (A0),D0".into(), 2));
        assert_eq!(dis(&[0xC190], 0), ("AND.L D0,(A0)".into(), 2));
        assert_eq!(dis(&[0xC0D0], 0), ("MULU (A0),D0".into(), 2));
        assert_eq!(dis(&[0xC1D0], 0), ("MULS (A0),D0".into(), 2));
        assert_eq!(dis(&[0xC101], 0), ("ABCD D1,D0".into(), 2));
        assert_eq!(dis(&[0xC109], 0), ("ABCD -(A1),-(A0)".into(), 2));
        assert_eq!(dis(&[0xC141], 0), ("EXG D0,D1".into(), 2));
        assert_eq!(dis(&[0xC149], 0), ("EXG A0,A1".into(), 2));
        assert_eq!(dis(&[0xC189], 0), ("EXG D0,A1".into(), 2));
        assert_eq!(dis(&[0xB2D0], 0), ("CMPA.W (A0),A1".into(), 2));
        assert_eq!(dis(&[0xB109], 0), ("CMPM.B (A1)+,(A0)+".into(), 2));
        assert_eq!(dis(&[0xB149], 0), ("CMPM.W (A1)+,(A0)+".into(), 2));
        assert_eq!(dis(&[0xB150], 0), ("EOR.W D0,(A0)".into(), 2));
    }

    #[test]
    fn unknown_is_dc_word() {
        assert_eq!(dis(&[0xA123], 0), ("DC.W $A123".into(), 2));
    }

    #[test]
    fn copper_move_wait_skip() {
        // MOVE #$0000,$DFF180 (COLOR00): reg offset 0x180, IR1 = 0x0180.
        assert_eq!(disassemble_copper(0x0180, 0x0123), "MOVE  #$0123,$DFF180");
        // WAIT for vp=0x2C,hp=0x00, all-ones mask, BFD set (ir2 bit15=1).
        // The decimal beam position matches the debugger's v/h coordinates.
        assert_eq!(
            disassemble_copper(0x2C01, 0xFFFE),
            "WAIT  vp=$2C,hp=$00 (v44 h0)"
        );
        // WAIT end of list (vp=0xFF,hp=0xFE), no BFD bit -> [BFD] note.
        let s = disassemble_copper(0xFFFF, 0xFFFE);
        assert!(s.starts_with("WAIT  vp=$FF,hp=$FE"), "{s}");
        // SKIP: ir2 bit0 set.
        assert!(disassemble_copper(0x2C01, 0xFFFF).starts_with("SKIP"));
    }
}
