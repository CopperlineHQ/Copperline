//! 68060 instruction timing: superscalar classification and pOEP cycle costs.
//!
//! The 68060 executes most integer instructions in one clock in the primary
//! operand-execution pipeline (pOEP); a restricted subset may dispatch to the
//! secondary pipeline (sOEP) in the same clock. MC68060UM Chapter 10 defines
//! the dispatch algorithm (Table 10-1) and classifies every instruction
//! (Tables 10-2/10-3); this module transcribes that classification as a pure
//! function of the opcode word, accelerated by a build-once 64K table.
//!
//! Costs returned here are pOEP occupancy assuming zero-wait operand access:
//! all memory latency is billed by the host bus at access time, so a cheap
//! count here never double-bills a bus access. The classification is
//! deliberately pessimistic where the manual's rules are finer than an
//! opcode-word decode can see (pessimism under-pairs; it never over-pairs).
//! The per-instruction constants are calibration knobs - see the timing-test
//! ADF rows and docs/internals/cpu.md.

use super::cpu::CpuCore;
use std::sync::OnceLock;

/// UM Tables 10-2/10-3 dispatch classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OepClass {
    /// May execute in either pipeline (the common 1-cycle instructions).
    PoepSoep = 0,
    /// Occupies the pOEP; nothing dispatches to the sOEP that cycle.
    PoepOnly = 1,
    /// Multi-cycle in the pOEP; an sOEP partner may join the final cycle
    /// (MOVEM is the canonical case).
    PoepUntilLast = 2,
    /// Must start in the pOEP but a pOEP|sOEP successor may still pair.
    PoepButAllowsSoep = 3,
}

/// Packed per-opcode timing entry: class(2) | cycles(5) | flags(9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info060(pub u16);

const CLASS_SHIFT: u16 = 0;
const CYCLES_SHIFT: u16 = 2;
const CYCLES_MASK: u16 = 0x1F;

/// Bcc/BRA/BSR: costed via the branch path (branch cache once it lands).
pub const F_BRANCH: u16 = 1 << 7;
/// DBcc: a branch with its own loop-mode cost.
pub const F_DBCC: u16 = 1 << 8;
/// Cost varies with data (divides, MOVEM, ...): derive from the handler's
/// raw 68000 count instead of the packed cycles field.
pub const F_VARIABLE: u16 = 1 << 9;
/// Instruction defines the CCR (partner CCR-consumers cannot pair behind it).
pub const F_DEFINES_CCR: u16 = 1 << 10;
/// Instruction consumes the CCR/X late (Scc, ADDX-style, DBcc).
pub const F_USES_CCR_LATE: u16 = 1 << 11;
/// Memory-indirect or indexed EA: +1 pOEP cycle, and never an sOEP candidate.
pub const F_EA_INDEXED: u16 = 1 << 12;
/// Not yet classified from the UM tables: pessimistic pOEP-only + raw fallback.
pub const F_UNCLASSIFIED: u16 = 1 << 13;
/// Instruction reads a memory operand.
pub const F_READS_MEM: u16 = 1 << 14;
/// Instruction writes a memory operand.
pub const F_WRITES_MEM: u16 = 1 << 15;

impl Info060 {
    const fn new(class: OepClass, cycles: u16, flags: u16) -> Self {
        Self((class as u16) << CLASS_SHIFT | (cycles & CYCLES_MASK) << CYCLES_SHIFT | flags)
    }

    pub fn class(self) -> OepClass {
        match (self.0 >> CLASS_SHIFT) & 3 {
            0 => OepClass::PoepSoep,
            1 => OepClass::PoepOnly,
            2 => OepClass::PoepUntilLast,
            _ => OepClass::PoepButAllowsSoep,
        }
    }

    pub fn cycles(self) -> i32 {
        i32::from((self.0 >> CYCLES_SHIFT) & CYCLES_MASK)
    }

    pub fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }
}

// Calibration knobs (all approximate pending timing-test/WinUAE cross-checks;
// see docs/internals/cpu.md "68060 timing").
/// Taken Bcc/BRA/BSR without branch-cache help: pipeline refill.
pub const CYC_060_BRANCH_TAKEN: i32 = 7;
/// Not-taken conditional branch.
pub const CYC_060_BRANCH_NOT_TAKEN: i32 = 1;
/// DBcc that loops (counter not expired, condition false).
pub const CYC_060_DBCC_TAKEN: i32 = 2;
/// DBcc that falls through.
pub const CYC_060_DBCC_EXPIRED: i32 = 3;
/// Floor for non-branch flow changes (JMP/JSR/RTS/RTE...): refill cost.
pub const CYC_060_FLOW_MIN: i32 = 5;

/// The 68060 rule of thumb for costs not in the table: a 4-clock 68000
/// register operation is 1 clock on the 060. Never reuse the 020+ scaling
/// formula here - its `.max(2)` floor would destroy 1-cycle costs.
#[inline]
fn fallback_cycles(raw: i32) -> i32 {
    (raw / 4).max(1)
}

/// Standard-EA helper: flags contributed by an effective-address field.
/// `read`/`write` say whether the instruction reads/writes that operand.
const fn ea_flags(mode: u16, reg: u16, read: bool, write: bool) -> u16 {
    let mut flags = 0;
    let is_mem = mode >= 2 && !(mode == 7 && reg == 4); // not Rn, not #imm
    if is_mem {
        if read {
            flags |= F_READS_MEM;
        }
        if write {
            flags |= F_WRITES_MEM;
        }
    }
    // Brief/full-format indexed and memory-indirect modes: (d8,An,Xn) and
    // (d8,PC,Xn) families. The opcode word cannot distinguish brief from
    // full extension words, so both are treated as indexed (pessimistic).
    if mode == 6 || (mode == 7 && reg == 3) {
        flags |= F_EA_INDEXED;
    }
    flags
}

/// One-cycle pOEP|sOEP ALU entry with standard EA flags.
const fn alu(mode: u16, reg: u16, read: bool, write: bool) -> Info060 {
    Info060::new(
        OepClass::PoepSoep,
        1,
        F_DEFINES_CCR | ea_flags(mode, reg, read, write),
    )
}

/// Classify one opcode word. Arranged in the same group order as
/// `decode::dispatch_instruction` so the two files can be reviewed side by
/// side. Unknown encodings return a pessimistic unclassified entry; illegal
/// encodings never reach the cost path (they trap).
pub fn classify_060_opcode(op: u16) -> Info060 {
    let group = op >> 12;
    let mode = (op >> 3) & 7;
    let reg = op & 7;
    let unclassified = Info060::new(OepClass::PoepOnly, 1, F_UNCLASSIFIED | F_VARIABLE);

    match group {
        0x0 => {
            if op & 0x0100 != 0 || (op & 0x0F00) == 0x0800 {
                // Bit ops BTST/BCHG/BCLR/BSET (dynamic and static #), MOVEP.
                // UM Table 10-2: bit operations are pOEP-only.
                Info060::new(
                    OepClass::PoepOnly,
                    1,
                    F_DEFINES_CCR | ea_flags(mode, reg, true, op & 0x00C0 != 0),
                )
            } else if (op & 0x0FC0) == 0x00C0 || (op & 0x09C0) == 0x08C0 {
                // CMP2/CHK2/CAS/CAS2/MOVES: pOEP-only, data-dependent.
                unclassified
            } else {
                // ORI/ANDI/SUBI/ADDI/EORI/CMPI #imm,<ea> (1 cycle, pOEP|sOEP);
                // the to-CCR/to-SR forms are privileged/serializing.
                if mode == 7 && reg == 4 {
                    Info060::new(OepClass::PoepOnly, 1, F_VARIABLE) // to CCR/SR
                } else {
                    alu(mode, reg, true, (op & 0x0F00) != 0x0C00) // CMPI never writes
                }
            }
        }
        // MOVE.B/L/W and MOVEA: 1 cycle, pOEP|sOEP. Source EA read plus
        // destination EA write; indexed penalty from either side.
        0x1 | 0x2 | 0x3 => {
            let dst_mode = (op >> 6) & 7;
            let dst_reg = (op >> 9) & 7;
            Info060::new(
                OepClass::PoepSoep,
                1,
                F_DEFINES_CCR
                    | ea_flags(mode, reg, true, false)
                    | ea_flags(dst_mode, dst_reg, false, true),
            )
        }
        0x4 => {
            match op & 0x0FC0 {
                // MOVE from SR / from CCR: serializing.
                0x00C0 | 0x02C0 => Info060::new(OepClass::PoepOnly, 1, F_VARIABLE),
                // MOVE to CCR / to SR.
                0x04C0 | 0x06C0 => Info060::new(OepClass::PoepOnly, 1, F_VARIABLE),
                _ => {
                    if (op & 0x0B80) == 0x0880 && mode != 0 {
                        // MOVEM: pOEP-until-last, one cycle per register plus
                        // setup - data-dependent, so raw fallback.
                        Info060::new(
                            OepClass::PoepUntilLast,
                            2,
                            F_VARIABLE | F_READS_MEM | F_WRITES_MEM,
                        )
                    } else if (op & 0x0FC0) == 0x0AC0 {
                        // TAS: locked RMW, pOEP-only.
                        Info060::new(
                            OepClass::PoepOnly,
                            2,
                            F_DEFINES_CCR | F_READS_MEM | F_WRITES_MEM,
                        )
                    } else if (op & 0x0F80) == 0x0C00 {
                        // MULL/DIVL (4C00/4C40): pOEP-only, data-dependent.
                        Info060::new(OepClass::PoepOnly, 2, F_DEFINES_CCR | F_VARIABLE)
                    } else if (op & 0x0FF8) == 0x0840 {
                        // SWAP
                        alu(0, 0, false, false)
                    } else if (op & 0x0E00) == 0x0800 && (op & 0x00C0) != 0x0040 {
                        // NBCD/EXT/EXTB (CLR handled below); EXT is pOEP|sOEP.
                        alu(mode, reg, true, true)
                    } else if (op & 0x0F00) == 0x0200
                        || (op & 0x0F00) == 0x0000
                        || (op & 0x0F00) == 0x0400
                        || (op & 0x0F00) == 0x0600
                    {
                        // NEGX/CLR/NEG/NOT <ea>: 1 cycle pOEP|sOEP.
                        alu(mode, reg, true, true)
                    } else if (op & 0x01C0) == 0x01C0 {
                        // LEA: 1 cycle, pOEP|sOEP (indexed forms pay +1).
                        Info060::new(OepClass::PoepSoep, 1, ea_flags(mode, reg, false, false))
                    } else if (op & 0x01C0) == 0x0180 {
                        // CHK: pOEP-only.
                        Info060::new(OepClass::PoepOnly, 2, F_VARIABLE)
                    } else {
                        // JSR/JMP/RTS/RTE/RTR/LINK/UNLK/PEA/TRAP/STOP/MOVEC/
                        // MOVE USP/TST... TST is common enough to special-case.
                        if (op & 0x0F00) == 0x0A00 {
                            // TST <ea>
                            alu(mode, reg, true, false)
                        } else {
                            // Control-flow and supervisor ops: pOEP-only; the
                            // flow-change floor in cycles_060 covers refills.
                            Info060::new(OepClass::PoepOnly, 1, F_VARIABLE)
                        }
                    }
                }
            }
        }
        0x5 => {
            if (op & 0x00F8) == 0x00C8 {
                // DBcc
                Info060::new(
                    OepClass::PoepOnly,
                    CYC_060_DBCC_EXPIRED as u16,
                    F_BRANCH | F_DBCC | F_USES_CCR_LATE,
                )
            } else if (op & 0x00C0) == 0x00C0 {
                // Scc <ea>: 1 cycle but consumes the CCR late.
                Info060::new(
                    OepClass::PoepSoep,
                    1,
                    F_USES_CCR_LATE | ea_flags(mode, reg, false, true),
                )
            } else {
                // ADDQ/SUBQ: 1 cycle, pOEP|sOEP.
                alu(mode, reg, true, true)
            }
        }
        0x6 => {
            // Bcc/BRA/BSR: branch path.
            Info060::new(
                OepClass::PoepOnly,
                CYC_060_BRANCH_TAKEN as u16,
                F_BRANCH | if (op & 0x0F00) != 0 { F_USES_CCR_LATE } else { 0 },
            )
        }
        0x7 => {
            // MOVEQ: the canonical 1-cycle pOEP|sOEP instruction.
            Info060::new(OepClass::PoepSoep, 1, F_DEFINES_CCR)
        }
        0x8 => {
            if (op & 0x00C0) == 0x00C0 {
                // DIVU.W/DIVS.W: pOEP-only, data-dependent.
                Info060::new(OepClass::PoepOnly, 2, F_DEFINES_CCR | F_VARIABLE)
            } else if (op & 0x01F0) == 0x0100 || (op & 0x01F0) == 0x0140 {
                // SBCD, PACK/UNPK: pOEP-only.
                Info060::new(OepClass::PoepOnly, 2, F_DEFINES_CCR | F_VARIABLE)
            } else {
                // OR
                alu(mode, reg, true, (op & 0x0100) != 0)
            }
        }
        0x9 | 0xD => {
            if (op & 0x0130) == 0x0100 && mode <= 1 {
                // ADDX/SUBX: consume X late, pOEP-only per UM.
                Info060::new(
                    OepClass::PoepOnly,
                    1,
                    F_DEFINES_CCR | F_USES_CCR_LATE | ea_flags(mode, reg, true, true),
                )
            } else {
                // ADD/SUB/ADDA/SUBA
                alu(mode, reg, true, (op & 0x0100) != 0 && (op & 0x00C0) != 0x00C0)
            }
        }
        0xB => {
            // CMP/CMPA/CMPM/EOR
            alu(mode, reg, true, (op & 0x0100) != 0 && (op & 0x00C0) != 0x00C0)
        }
        0xC => {
            if (op & 0x00C0) == 0x00C0 {
                // MULU.W/MULS.W: 2 cycles, pOEP-only.
                Info060::new(OepClass::PoepOnly, 2, F_DEFINES_CCR)
            } else if (op & 0x01F0) == 0x0100 {
                // ABCD
                Info060::new(OepClass::PoepOnly, 2, F_DEFINES_CCR | F_VARIABLE)
            } else if (op & 0x01F8) == 0x0140 || (op & 0x01F8) == 0x0148 || (op & 0x01F8) == 0x0188
            {
                // EXG: pOEP-only per UM.
                Info060::new(OepClass::PoepOnly, 1, 0)
            } else {
                // AND
                alu(mode, reg, true, (op & 0x0100) != 0)
            }
        }
        0xE => {
            if (op & 0x08C0) == 0x08C0 {
                // Bitfields: pOEP-only, data-dependent.
                unclassified
            } else if (op & 0x00C0) == 0x00C0 {
                // Memory shifts (single bit): 1 cycle.
                Info060::new(
                    OepClass::PoepSoep,
                    1,
                    F_DEFINES_CCR | F_READS_MEM | F_WRITES_MEM,
                )
            } else if (op & 0x0018) == 0x0010 {
                // ROXL/ROXR: consume X, pOEP-only.
                Info060::new(OepClass::PoepOnly, 1, F_DEFINES_CCR | F_USES_CCR_LATE)
            } else {
                // Register shifts/rotates: 1 cycle, pOEP|sOEP per UM.
                Info060::new(OepClass::PoepSoep, 1, F_DEFINES_CCR)
            }
        }
        // A-line, F-line (FPU/MMU/MOVE16/LPSTOP), and anything else: pOEP-only
        // with data-dependent cost.
        _ => unclassified,
    }
}

/// The 64K classification table, built once per process. Pure function of
/// the opcode word; never serialized.
fn info_table() -> &'static [u16; 0x10000] {
    static TABLE: OnceLock<Box<[u16; 0x10000]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![0u16; 0x10000];
        for (op, slot) in table.iter_mut().enumerate() {
            *slot = classify_060_opcode(op as u16).0;
        }
        table.into_boxed_slice().try_into().unwrap()
    })
}

/// Look up the packed timing entry for an opcode.
#[inline]
pub fn info_060(op: u16) -> Info060 {
    Info060(info_table()[op as usize])
}

impl CpuCore {
    /// 68060 cycle cost for the instruction that just retired normally
    /// (exception entries keep the handlers' own costs). `raw` is the
    /// handler's 68000-reference count, used for data-dependent fallbacks.
    pub(crate) fn cycles_060(&mut self, raw: i32) -> i32 {
        let info = info_060(self.ir as u16);
        let flowed = self.change_of_flow;

        if info.has(F_BRANCH) {
            // Static branch costs; the branch-cache model replaces these for
            // Bcc/BRA/BSR when EBC is enabled.
            return if info.has(F_DBCC) {
                // The DBcc handler does not raise change_of_flow; a loop is
                // visible as a PC that is not the fall-through (ppc + 4).
                if self.pc != self.ppc.wrapping_add(4) {
                    CYC_060_DBCC_TAKEN
                } else {
                    CYC_060_DBCC_EXPIRED
                }
            } else if flowed {
                CYC_060_BRANCH_TAKEN
            } else {
                CYC_060_BRANCH_NOT_TAKEN
            };
        }
        if flowed {
            // JMP/JSR/RTS/RTE/RTR and friends: pipeline refill floor.
            return fallback_cycles(raw).max(CYC_060_FLOW_MIN);
        }
        let mut cycles = if info.has(F_VARIABLE) || info.has(F_UNCLASSIFIED) {
            fallback_cycles(raw)
        } else {
            info.cycles()
        };
        if info.has(F_EA_INDEXED) {
            cycles += 1;
        }
        cycles
    }

    /// Model-dispatching wrapper for the three step paths: the 68060 uses its
    /// own cost model; every other model keeps the existing scaling
    /// byte-for-byte.
    #[inline]
    pub(crate) fn finalize_cycles(&mut self, raw: i32) -> i32 {
        if self.cpu_type == super::types::CpuType::M68060 {
            self.cycles_060(raw)
        } else {
            self.scale_cycles_for_cpu_type(raw)
        }
    }
}
