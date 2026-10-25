//! Instruction disassembly/formatting.
//!
//! Provides a human-readable summary of one M68000-family opcode word.

use crate::core::types::CpuType;

/// Register names for data registers
const DN: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
/// Register names for address registers
const AN: [&str; 8] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];
/// Condition codes for Bcc/Scc/DBcc
const CC: [&str; 16] = [
    "T", "F", "HI", "LS", "CC", "CS", "NE", "EQ", "VC", "VS", "PL", "MI", "GE", "LT", "GT", "LE",
];

/// Size suffix for opcodes
fn size_suffix(size: u8) -> &'static str {
    match size {
        0 => ".B",
        1 => ".W",
        2 => ".L",
        _ => "",
    }
}

/// Disassemble a single instruction.
///
/// Returns the formatted mnemonic and an estimated instruction size in bytes.
///
/// Only `opcode` is decoded. Instructions that require extension words use
/// placeholders such as `#xx` or `<ea>`, so the returned size cannot account
/// for every effective-address extension. `cpu_type` selects model-specific
/// F-line formatting; `pc` is reserved for future relative-address rendering.
pub fn disassemble(_pc: u32, opcode: u16, cpu_type: CpuType) -> (String, u32) {
    let op_hi = (opcode >> 12) & 0xF;

    match op_hi {
        // 0x0: Bit manipulation / MOVEP / Immediate
        0x0 => disasm_0xxx(opcode),

        // 0x1: MOVE.B
        0x1 => disasm_move(opcode, 0),

        // 0x2: MOVE.L
        0x2 => disasm_move(opcode, 2),

        // 0x3: MOVE.W
        0x3 => disasm_move(opcode, 1),

        // 0x4: Miscellaneous
        0x4 => disasm_4xxx(opcode),

        // 0x5: ADDQ/SUBQ/Scc/DBcc
        0x5 => disasm_5xxx(opcode),

        // 0x6: Bcc/BSR/BRA
        0x6 => disasm_branch(opcode),

        // 0x7: MOVEQ
        0x7 => {
            let reg = ((opcode >> 9) & 7) as usize;
            let data = (opcode & 0xFF) as i8;
            (format!("MOVEQ #{},{}", data, DN[reg]), 2)
        }

        // 0x8: OR/DIV/SBCD
        0x8 => disasm_8xxx(opcode),

        // 0x9: SUB/SUBA/SUBX
        0x9 => disasm_9xxx(opcode),

        // 0xA: Line-A (A-line trap)
        0xA => (format!("DC.W ${:04X}", opcode), 2), // A-line trap

        // 0xB: CMP/EOR
        0xB => disasm_bxxx(opcode),

        // 0xC: AND/MUL/ABCD/EXG
        0xC => disasm_cxxx(opcode),

        // 0xD: ADD/ADDA/ADDX
        0xD => disasm_dxxx(opcode),

        // 0xE: Shift/Rotate/Bit Field
        0xE => disasm_shift(opcode),

        // 0xF: Coprocessor/FPU (F-line)
        0xF => disasm_fline(opcode, cpu_type),

        _ => (format!("DC.W ${:04X}", opcode), 2),
    }
}

/// Disassemble 0xxx opcodes (ORI, ANDI, SUBI, etc.)
fn disasm_0xxx(opcode: u16) -> (String, u32) {
    let size = ((opcode >> 6) & 3) as u8;

    // Check for immediate operations
    match (opcode >> 8) & 0xF {
        0x0 => {
            if opcode == 0x003C {
                return ("ORI.B #xx,CCR".to_string(), 4);
            }
            return (format!("ORI{} #xx,<ea>", size_suffix(size)), 4);
        }
        0x2 => return (format!("ANDI{} #xx,<ea>", size_suffix(size)), 4),
        0x4 => return (format!("SUBI{} #xx,<ea>", size_suffix(size)), 4),
        0x6 => return (format!("ADDI{} #xx,<ea>", size_suffix(size)), 4),
        0x8 => {
            // BTST/BCHG/BCLR/BSET immediate
            let bit_op = (opcode >> 6) & 3;
            let op_name = match bit_op {
                0 => "BTST",
                1 => "BCHG",
                2 => "BCLR",
                3 => "BSET",
                _ => "???",
            };
            return (format!("{} #xx,<ea>", op_name), 4);
        }
        0xA => return (format!("EORI{} #xx,<ea>", size_suffix(size)), 4),
        0xC => return (format!("CMPI{} #xx,<ea>", size_suffix(size)), 4),
        _ => {}
    }

    // Check for MOVEP
    if (opcode & 0x0138) == 0x0108 {
        return ("MOVEP <ea>,Dn".to_string(), 4);
    }

    // Bit operations with register
    let bit_op = (opcode >> 6) & 3;
    let reg = ((opcode >> 9) & 7) as usize;
    let op_name = match bit_op {
        0 => "BTST",
        1 => "BCHG",
        2 => "BCLR",
        3 => "BSET",
        _ => "???",
    };
    (format!("{} {},<ea>", op_name, DN[reg]), 2)
}

/// Disassemble MOVE instruction
fn disasm_move(opcode: u16, size: u8) -> (String, u32) {
    let src_mode = (opcode >> 3) & 7;
    let src_reg = opcode & 7;
    let dst_reg = (opcode >> 9) & 7;
    let dst_mode = (opcode >> 6) & 7;

    let src = format_ea(src_mode as u8, src_reg as u8, size);
    let dst = format_ea(dst_mode as u8, dst_reg as u8, size);

    (format!("MOVE{} {},{}", size_suffix(size), src, dst), 2)
}

/// Disassemble 4xxx opcodes (miscellaneous)
fn disasm_4xxx(opcode: u16) -> (String, u32) {
    // NOP
    if opcode == 0x4E71 {
        return ("NOP".to_string(), 2);
    }

    // RTS
    if opcode == 0x4E75 {
        return ("RTS".to_string(), 2);
    }

    // RTE
    if opcode == 0x4E73 {
        return ("RTE".to_string(), 2);
    }

    // RTR
    if opcode == 0x4E77 {
        return ("RTR".to_string(), 2);
    }

    // STOP
    if opcode == 0x4E72 {
        return ("STOP #xxxx".to_string(), 4);
    }

    // RESET
    if opcode == 0x4E70 {
        return ("RESET".to_string(), 2);
    }

    // TRAP
    if (opcode & 0xFFF0) == 0x4E40 {
        let vector = opcode & 0xF;
        return (format!("TRAP #{}", vector), 2);
    }

    // LINK
    if (opcode & 0xFFF8) == 0x4E50 {
        let reg = (opcode & 7) as usize;
        return (format!("LINK {},#xxxx", AN[reg]), 4);
    }

    // UNLK
    if (opcode & 0xFFF8) == 0x4E58 {
        let reg = (opcode & 7) as usize;
        return (format!("UNLK {}", AN[reg]), 2);
    }

    // MOVE USP
    if (opcode & 0xFFF0) == 0x4E60 {
        let reg = (opcode & 7) as usize;
        let dir = (opcode >> 3) & 1;
        if dir == 0 {
            return (format!("MOVE {},USP", AN[reg]), 2);
        } else {
            return (format!("MOVE USP,{}", AN[reg]), 2);
        }
    }

    // JMP/JSR
    if (opcode & 0xFFC0) == 0x4EC0 {
        return ("JMP <ea>".to_string(), 2);
    }
    if (opcode & 0xFFC0) == 0x4E80 {
        return ("JSR <ea>".to_string(), 2);
    }

    // LEA
    if (opcode & 0xF1C0) == 0x41C0 {
        let reg = ((opcode >> 9) & 7) as usize;
        return (format!("LEA <ea>,{}", AN[reg]), 2);
    }

    // PEA
    if (opcode & 0xFFC0) == 0x4840 {
        return ("PEA <ea>".to_string(), 2);
    }

    // MOVEM
    if (opcode & 0xFB80) == 0x4880 {
        let dir = (opcode >> 10) & 1;
        let size = if (opcode >> 6) & 1 == 0 { ".W" } else { ".L" };
        if dir == 0 {
            return (format!("MOVEM{} <regs>,<ea>", size), 4);
        } else {
            return (format!("MOVEM{} <ea>,<regs>", size), 4);
        }
    }

    // CLR/NEG/NEGX/NOT/TST
    let size = ((opcode >> 6) & 3) as u8;
    match (opcode >> 8) & 0xF {
        0x2 => return (format!("CLR{} <ea>", size_suffix(size)), 2),
        0x4 => return (format!("NEG{} <ea>", size_suffix(size)), 2),
        0x0 => return (format!("NEGX{} <ea>", size_suffix(size)), 2),
        0x6 => return (format!("NOT{} <ea>", size_suffix(size)), 2),
        0xA => return (format!("TST{} <ea>", size_suffix(size)), 2),
        _ => {}
    }

    // EXT
    if (opcode & 0xFEB8) == 0x4880 {
        let reg = (opcode & 7) as usize;
        let size = if (opcode >> 6) & 1 == 0 { ".W" } else { ".L" };
        return (format!("EXT{} {}", size, DN[reg]), 2);
    }

    // SWAP
    if (opcode & 0xFFF8) == 0x4840 {
        let reg = (opcode & 7) as usize;
        return (format!("SWAP {}", DN[reg]), 2);
    }

    // CHK
    if (opcode & 0xF1C0) == 0x4180 {
        let reg = ((opcode >> 9) & 7) as usize;
        return (format!("CHK <ea>,{}", DN[reg]), 2);
    }

    (format!("DC.W ${:04X}", opcode), 2)
}

/// Disassemble 5xxx opcodes (ADDQ/SUBQ/Scc/DBcc)
fn disasm_5xxx(opcode: u16) -> (String, u32) {
    let size = ((opcode >> 6) & 3) as u8;

    // Scc/DBcc
    if size == 3 {
        let mode = (opcode >> 3) & 7;
        let cond = ((opcode >> 8) & 0xF) as usize;

        if mode == 1 {
            // DBcc
            let reg = (opcode & 7) as usize;
            return (format!("DB{} {},<label>", CC[cond], DN[reg]), 4);
        } else {
            // Scc
            return (format!("S{} <ea>", CC[cond]), 2);
        }
    }

    // ADDQ/SUBQ
    let data = ((opcode >> 9) & 7) as u8;
    let data = if data == 0 { 8 } else { data };
    let op = if (opcode >> 8) & 1 == 0 {
        "ADDQ"
    } else {
        "SUBQ"
    };

    (format!("{}{} #{},<ea>", op, size_suffix(size), data), 2)
}

/// Disassemble branch instructions (Bcc/BSR/BRA)
fn disasm_branch(opcode: u16) -> (String, u32) {
    let cond = ((opcode >> 8) & 0xF) as usize;
    let disp = (opcode & 0xFF) as i8;

    let mnemonic = match cond {
        0 => "BRA",
        1 => "BSR",
        _ => {
            return (
                format!("B{} <label>", CC[cond]),
                if disp == 0 { 4 } else { 2 },
            );
        }
    };

    if disp == 0 {
        (format!("{}.W <label>", mnemonic), 4)
    } else {
        (format!("{}.S <label>", mnemonic), 2)
    }
}

/// Disassemble 8xxx opcodes (OR/DIV/SBCD)
fn disasm_8xxx(opcode: u16) -> (String, u32) {
    let reg = ((opcode >> 9) & 7) as usize;
    let size = ((opcode >> 6) & 3) as u8;
    let opmode = (opcode >> 6) & 7;

    // SBCD
    if opmode == 4 {
        return ("SBCD <ea>,<ea>".to_string(), 2);
    }

    // DIVS/DIVU
    if opmode == 7 {
        return (format!("DIVS.W <ea>,{}", DN[reg]), 2);
    }
    if opmode == 3 {
        return (format!("DIVU.W <ea>,{}", DN[reg]), 2);
    }

    // OR
    if (opcode >> 8) & 1 == 0 {
        (format!("OR{} <ea>,{}", size_suffix(size), DN[reg]), 2)
    } else {
        (format!("OR{} {},<ea>", size_suffix(size), DN[reg]), 2)
    }
}

/// Disassemble 9xxx opcodes (SUB/SUBA/SUBX)
fn disasm_9xxx(opcode: u16) -> (String, u32) {
    let reg = ((opcode >> 9) & 7) as usize;
    let size = ((opcode >> 6) & 3) as u8;
    let opmode = (opcode >> 6) & 7;

    // SUBA
    if opmode == 3 || opmode == 7 {
        let sz = if opmode == 3 { ".W" } else { ".L" };
        return (format!("SUBA{} <ea>,{}", sz, AN[reg]), 2);
    }

    // SUBX
    if (opcode & 0x0130) == 0x0100 && size != 3 {
        return (format!("SUBX{} <ea>,<ea>", size_suffix(size)), 2);
    }

    // SUB
    if (opcode >> 8) & 1 == 0 {
        (format!("SUB{} <ea>,{}", size_suffix(size), DN[reg]), 2)
    } else {
        (format!("SUB{} {},<ea>", size_suffix(size), DN[reg]), 2)
    }
}

/// Disassemble Bxxx opcodes (CMP/EOR/CMPM)
fn disasm_bxxx(opcode: u16) -> (String, u32) {
    let reg = ((opcode >> 9) & 7) as usize;
    let size = ((opcode >> 6) & 3) as u8;
    let opmode = (opcode >> 6) & 7;

    // CMPA
    if opmode == 3 || opmode == 7 {
        let sz = if opmode == 3 { ".W" } else { ".L" };
        return (format!("CMPA{} <ea>,{}", sz, AN[reg]), 2);
    }

    // CMPM
    if (opcode & 0x0138) == 0x0108 {
        return (format!("CMPM{} (Ay)+,(Ax)+", size_suffix(size)), 2);
    }

    // EOR
    if (opcode >> 8) & 1 == 1 {
        return (format!("EOR{} {},<ea>", size_suffix(size), DN[reg]), 2);
    }

    // CMP
    (format!("CMP{} <ea>,{}", size_suffix(size), DN[reg]), 2)
}

/// Disassemble Cxxx opcodes (AND/MUL/ABCD/EXG)
fn disasm_cxxx(opcode: u16) -> (String, u32) {
    let reg = ((opcode >> 9) & 7) as usize;
    let size = ((opcode >> 6) & 3) as u8;
    let opmode = (opcode >> 6) & 7;

    // ABCD
    if opmode == 4 {
        return ("ABCD <ea>,<ea>".to_string(), 2);
    }

    // EXG
    if (opcode & 0xF130) == 0xC100 {
        return ("EXG Rx,Ry".to_string(), 2);
    }

    // MULS/MULU
    if opmode == 7 {
        return (format!("MULS.W <ea>,{}", DN[reg]), 2);
    }
    if opmode == 3 {
        return (format!("MULU.W <ea>,{}", DN[reg]), 2);
    }

    // AND
    if (opcode >> 8) & 1 == 0 {
        (format!("AND{} <ea>,{}", size_suffix(size), DN[reg]), 2)
    } else {
        (format!("AND{} {},<ea>", size_suffix(size), DN[reg]), 2)
    }
}

/// Disassemble Dxxx opcodes (ADD/ADDA/ADDX)
fn disasm_dxxx(opcode: u16) -> (String, u32) {
    let reg = ((opcode >> 9) & 7) as usize;
    let size = ((opcode >> 6) & 3) as u8;
    let opmode = (opcode >> 6) & 7;

    // ADDA
    if opmode == 3 || opmode == 7 {
        let sz = if opmode == 3 { ".W" } else { ".L" };
        return (format!("ADDA{} <ea>,{}", sz, AN[reg]), 2);
    }

    // ADDX
    if (opcode & 0x0130) == 0x0100 && size != 3 {
        return (format!("ADDX{} <ea>,<ea>", size_suffix(size)), 2);
    }

    // ADD
    if (opcode >> 8) & 1 == 0 {
        (format!("ADD{} <ea>,{}", size_suffix(size), DN[reg]), 2)
    } else {
        (format!("ADD{} {},<ea>", size_suffix(size), DN[reg]), 2)
    }
}

/// Disassemble shift/rotate instructions
fn disasm_shift(opcode: u16) -> (String, u32) {
    let size = ((opcode >> 6) & 3) as u8;

    // Memory shifts
    if size == 3 {
        let op = match (opcode >> 9) & 7 {
            0 => "ASR",
            1 => "ASL",
            2 => "LSR",
            3 => "LSL",
            4 => "ROXR",
            5 => "ROXL",
            6 => "ROR",
            7 => "ROL",
            _ => "???",
        };
        return (format!("{} <ea>", op), 2);
    }

    // Register shifts
    let ir = (opcode >> 5) & 1; // Immediate/Register
    let dr = (opcode >> 8) & 1; // Direction (0=right, 1=left)
    let op_type = (opcode >> 3) & 3;

    let op_base = match op_type {
        0 => {
            if dr == 0 {
                "ASR"
            } else {
                "ASL"
            }
        }
        1 => {
            if dr == 0 {
                "LSR"
            } else {
                "LSL"
            }
        }
        2 => {
            if dr == 0 {
                "ROXR"
            } else {
                "ROXL"
            }
        }
        3 => {
            if dr == 0 {
                "ROR"
            } else {
                "ROL"
            }
        }
        _ => "???",
    };

    let reg = (opcode & 7) as usize;
    let cnt = ((opcode >> 9) & 7) as u8;

    if ir == 0 {
        let cnt = if cnt == 0 { 8 } else { cnt };
        (
            format!("{}{} #{},{}", op_base, size_suffix(size), cnt, DN[reg]),
            2,
        )
    } else {
        let cnt_reg = cnt as usize;
        (
            format!(
                "{}{} {},{}",
                op_base,
                size_suffix(size),
                DN[cnt_reg],
                DN[reg]
            ),
            2,
        )
    }
}

/// Disassemble F-line (coprocessor/FPU) instructions
fn disasm_fline(opcode: u16, cpu_type: CpuType) -> (String, u32) {
    // Check if this is an FPU instruction
    let cp_id = (opcode >> 9) & 7;

    // CP ID 1 = FPU (68881/68882/68040 FPU)
    if cp_id == 1 {
        // Check for 68020+ or 68040
        match cpu_type {
            CpuType::M68EC020
            | CpuType::M68020
            | CpuType::M68EC030
            | CpuType::M68030
            | CpuType::M68EC040
            | CpuType::M68LC040
            | CpuType::M68040 => {
                // This is an FPU instruction
                let cmd = (opcode >> 6) & 7;
                match cmd {
                    0 => return ("FMOVE/FINT/... <ea>,FPn".to_string(), 4),
                    1 => return ("FBcc <label>".to_string(), 4),
                    2 => return ("FMOVEM <ea>,<list>".to_string(), 4),
                    3 => return ("FMOVE FPn,<ea>".to_string(), 4),
                    4 => return ("FMOVEM <list>,<ea>".to_string(), 4),
                    5 => return ("FMOVE.L <ea>,FPCR/FPSR/FPIAR".to_string(), 4),
                    6 => return ("FMOVE.L FPCR/FPSR/FPIAR,<ea>".to_string(), 4),
                    7 => return ("FDBcc/FScc/FTRAPcc".to_string(), 4),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // CP ID 0 = MMU
    if cp_id == 0 {
        return ("PMOVE/PTEST/...".to_string(), 4);
    }

    // Unknown F-line
    (format!("DC.W ${:04X}", opcode), 2)
}

/// Format an effective address
fn format_ea(mode: u8, reg: u8, _size: u8) -> String {
    let reg = reg as usize;
    match mode {
        0 => DN[reg].to_string(),           // Dn
        1 => AN[reg].to_string(),           // An
        2 => format!("({})", AN[reg]),      // (An)
        3 => format!("({})+", AN[reg]),     // (An)+
        4 => format!("-({})", AN[reg]),     // -(An)
        5 => format!("d16({})", AN[reg]),   // d16(An)
        6 => format!("d8({},Xi)", AN[reg]), // d8(An,Xi)
        7 => match reg {
            0 => "(xxx).W".to_string(),   // Absolute short
            1 => "(xxx).L".to_string(),   // Absolute long
            2 => "d16(PC)".to_string(),   // PC relative
            3 => "d8(PC,Xi)".to_string(), // PC relative indexed
            4 => "#<data>".to_string(),   // Immediate
            _ => "???".to_string(),
        },
        _ => "???".to_string(),
    }
}
