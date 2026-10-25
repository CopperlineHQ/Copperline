//! Fastmem-decoded memory-operand instructions for the batch loop.
//!
//! These ops execute against a contiguous RAM window exposed by the bus
//! ([`AddressBus::fast_mem`](super::memory::AddressBus::fast_mem)). They are
//! used only by the instruction-budgeted [`CpuCore::run_batch`] loop: the
//! cycle-budgeted paths never populate the window, so cache entries tagged
//! `Mem` fall back to full dispatch there and cycle accounting is unaffected.
//!
//! Correctness rules:
//! - An op either executes completely or reports "not handled" with **zero**
//!   state changes (registers, flags, memory, PC) — the caller then runs the
//!   same opcode through full dispatch, which handles faults, address errors
//!   and cycle-exact semantics.
//! - All guards (window range, pre-68020 alignment) run before any commit.
//!   Window reads are side-effect-free per the bus contract, so a read
//!   followed by fallback is harmless.
//! - Effective addresses are masked with `CpuCore::address` before the
//!   window check, exactly like bus accesses in the interpreter.

use super::cpu::CpuCore;
use super::op_cache::{AddrOp, BinaryOp, BitOp, is_pre_68020};
use super::types::{CpuType, Size};

/// Effective-address forms the fastmem path understands. Everything else
/// (68020 full-format indexing, register pairs, …) falls back to dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FastEa {
    DataReg(u8),
    AddrReg(u8),
    /// (An)
    AnInd(u8),
    /// (An)+
    AnPostInc(u8),
    /// -(An)
    AnPreDec(u8),
    /// (d16,An)
    AnDisp(u8),
    /// (d8,An,Xn) — brief extension word only
    AnIndex(u8),
    /// (xxx).W
    AbsW,
    /// (xxx).L
    AbsL,
    /// (d16,PC)
    PcDisp,
    /// (d8,PC,Xn) — brief extension word only
    PcIndex,
    /// #imm
    Imm,
}

impl FastEa {
    #[inline]
    fn decode(mode: u16, reg: u16) -> Option<FastEa> {
        Some(match mode & 7 {
            0 => FastEa::DataReg(reg as u8),
            1 => FastEa::AddrReg(reg as u8),
            2 => FastEa::AnInd(reg as u8),
            3 => FastEa::AnPostInc(reg as u8),
            4 => FastEa::AnPreDec(reg as u8),
            5 => FastEa::AnDisp(reg as u8),
            6 => FastEa::AnIndex(reg as u8),
            7 => match reg & 7 {
                0 => FastEa::AbsW,
                1 => FastEa::AbsL,
                2 => FastEa::PcDisp,
                3 => FastEa::PcIndex,
                4 => FastEa::Imm,
                _ => return None,
            },
            _ => unreachable!(),
        })
    }

    /// Data-alterable EA: legal destination for MOVE/CLR/ALU-to-mem/….
    #[inline]
    fn is_data_alterable(self) -> bool {
        !matches!(
            self,
            FastEa::AddrReg(_) | FastEa::PcDisp | FastEa::PcIndex | FastEa::Imm
        )
    }

    /// Memory EA usable as a control address (LEA/PEA/JMP/JSR).
    #[inline]
    fn is_control(self) -> bool {
        matches!(
            self,
            FastEa::AnInd(_)
                | FastEa::AnDisp(_)
                | FastEa::AnIndex(_)
                | FastEa::AbsW
                | FastEa::AbsL
                | FastEa::PcDisp
                | FastEa::PcIndex
        )
    }

    #[inline]
    fn is_memory(self) -> bool {
        !matches!(self, FastEa::DataReg(_) | FastEa::AddrReg(_) | FastEa::Imm)
    }
}

/// Bit-number source for the BTST/BCHG/BCLR/BSET memory forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitSource {
    Reg(u8),
    /// Bit number in an immediate extension word (read before the EA words).
    Imm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodedMemOp {
    /// MOVE/MOVEA. `dst == AddrReg` is MOVEA (sign-extends word, no flags).
    Move {
        size: Size,
        src: FastEa,
        dst: FastEa,
    },
    /// `MOVEM.W (An)+,<register-list>`. The fast path accepts data-register-
    /// only masks at execution time; other lists fall back atomically.
    MovemWordPostInc {
        base: u8,
    },
    BitImmDataReg {
        op: BitOp,
        reg: u8,
    },
    Link {
        reg: u8,
    },
    Unlk {
        reg: u8,
    },
    MovemLongPreDec {
        base: u8,
    },
    MovemLongPostInc {
        base: u8,
    },
    /// ADD/SUB/AND/OR/CMP `<ea>,Dn`
    AluToReg {
        op: BinaryOp,
        size: Size,
        src: FastEa,
        dst: u8,
    },
    /// ADD/SUB/AND/OR/EOR `Dn,<ea>` (memory-alterable EA)
    AluToMem {
        op: BinaryOp,
        size: Size,
        src: u8,
        dst: FastEa,
    },
    /// ADDA/SUBA/CMPA `<ea>,An`
    AluAddr {
        op: AddrOp,
        size: Size,
        src: FastEa,
        dst: u8,
    },
    /// ADDI/SUBI/ANDI/ORI/EORI/CMPI `#imm,<ea>`
    AluImm {
        op: BinaryOp,
        size: Size,
        dst: FastEa,
    },
    /// ADDQ/SUBQ `#q,<ea>` (memory EA; register forms are simple ops)
    AddqSubq {
        data: u32,
        size: Size,
        ea: FastEa,
        is_sub: bool,
    },
    Tst {
        size: Size,
        ea: FastEa,
    },
    Clr {
        size: Size,
        ea: FastEa,
    },
    Neg {
        size: Size,
        ea: FastEa,
    },
    Not {
        size: Size,
        ea: FastEa,
    },
    /// BTST/BCHG/BCLR/BSET on a memory byte.
    BitMem {
        op: BitOp,
        bit: BitSource,
        ea: FastEa,
    },
    /// CMPM (Ay)+,(Ax)+
    CmpM {
        size: Size,
        src: u8,
        dst: u8,
    },
    Lea {
        reg: u8,
        ea: FastEa,
    },
    Pea {
        ea: FastEa,
    },
    Jmp {
        ea: FastEa,
    },
    Jsr {
        ea: FastEa,
    },
    Rts,
    /// BSR with byte or word displacement (`length` covers opcode + ext).
    Bsr {
        displacement: i32,
        length: u32,
    },
    /// Bcc/BRA with word displacement.
    BranchWord {
        condition: u8,
    },
    Dbcc {
        condition: u8,
        reg: u8,
    },
}

// ============================================================================
// Decode
// ============================================================================

impl DecodedMemOp {
    pub(crate) fn decode(cpu_type: CpuType, opcode: u16) -> Option<Self> {
        let group = (opcode >> 12) & 0xF;
        match group {
            0x0 => decode_group_0(opcode),
            0x1 => decode_move(opcode, Size::Byte),
            0x2 => decode_move(opcode, Size::Long),
            0x3 => decode_move(opcode, Size::Word),
            0x4 => decode_group_4(cpu_type, opcode),
            0x5 => decode_group_5(opcode),
            0x6 => decode_group_6(opcode),
            0x8 | 0x9 | 0xB | 0xC | 0xD => decode_alu(opcode),
            _ => None,
        }
    }
}

#[inline]
fn decode_size_00(bits: u16) -> Option<Size> {
    match bits {
        0 => Some(Size::Byte),
        1 => Some(Size::Word),
        2 => Some(Size::Long),
        _ => None,
    }
}

fn decode_move(opcode: u16, size: Size) -> Option<DecodedMemOp> {
    let src = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
    let dst = FastEa::decode((opcode >> 6) & 7, (opcode >> 9) & 7)?;

    // An as byte source is illegal; MOVEA.B does not exist.
    if size == Size::Byte
        && (matches!(src, FastEa::AddrReg(_)) || matches!(dst, FastEa::AddrReg(_)))
    {
        return None;
    }
    if !dst.is_data_alterable() && !matches!(dst, FastEa::AddrReg(_)) {
        return None;
    }
    // Register-to-register MOVEs are covered by the simple-op path; only
    // accept forms that touch memory or read extension/immediate words.
    Some(DecodedMemOp::Move { size, src, dst })
}

fn decode_group_0(opcode: u16) -> Option<DecodedMemOp> {
    // Dynamic bit ops: 0000 rrr1 xxmm mrrr with memory EA.
    if (opcode & 0x0100) != 0 {
        let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
        // MOVEP shares this encoding with mode 1 (An).
        if !ea.is_memory() || !ea.is_data_alterable() {
            return None;
        }
        let op = match (opcode >> 6) & 3 {
            0 => BitOp::Test,
            1 => BitOp::Change,
            2 => BitOp::Clear,
            3 => BitOp::Set,
            _ => unreachable!(),
        };
        return Some(DecodedMemOp::BitMem {
            op,
            bit: BitSource::Reg(((opcode >> 9) & 7) as u8),
            ea,
        });
    }

    // Static bit ops: 0000 1000 xxmm mrrr (#imm form).
    if (opcode & 0xFF00) == 0x0800 {
        if (opcode >> 3) & 7 == 0 {
            let op = match (opcode >> 6) & 3 {
                0 => BitOp::Test,
                1 => BitOp::Change,
                2 => BitOp::Clear,
                3 => BitOp::Set,
                _ => unreachable!(),
            };
            return Some(DecodedMemOp::BitImmDataReg {
                op,
                reg: (opcode & 7) as u8,
            });
        }
        let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
        if !ea.is_memory() || !ea.is_data_alterable() {
            return None;
        }
        let op = match (opcode >> 6) & 3 {
            0 => BitOp::Test,
            1 => BitOp::Change,
            2 => BitOp::Clear,
            3 => BitOp::Set,
            _ => unreachable!(),
        };
        return Some(DecodedMemOp::BitMem {
            op,
            bit: BitSource::Imm,
            ea,
        });
    }

    // Immediate ALU: 0000 ooo0 ssmm mrrr.
    let size = decode_size_00((opcode >> 6) & 3)?;
    let op = match (opcode >> 9) & 7 {
        0 => BinaryOp::Or,
        1 => BinaryOp::And,
        2 => BinaryOp::Sub,
        3 => BinaryOp::Add,
        5 => BinaryOp::Eor,
        6 => BinaryOp::Cmp,
        _ => return None,
    };
    let dst = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
    // Excludes An (illegal), #imm (this is ORI-to-CCR/SR space) and PC-rel
    // destinations; CMPI on PC-relative EAs (68020+) also falls back.
    if !dst.is_data_alterable() {
        return None;
    }
    Some(DecodedMemOp::AluImm { op, size, dst })
}

fn decode_group_4(cpu_type: CpuType, opcode: u16) -> Option<DecodedMemOp> {
    if opcode == 0x4E75 {
        return Some(DecodedMemOp::Rts);
    }

    if (opcode & 0xFFF8) == 0x4C98 {
        return Some(DecodedMemOp::MovemWordPostInc {
            base: (opcode & 7) as u8,
        });
    }
    if (opcode & 0xFFF8) == 0x4E50 && (opcode & 7) != 7 {
        return Some(DecodedMemOp::Link {
            reg: (opcode & 7) as u8,
        });
    }
    if (opcode & 0xFFF8) == 0x4E58 && (opcode & 7) != 7 {
        return Some(DecodedMemOp::Unlk {
            reg: (opcode & 7) as u8,
        });
    }
    if (opcode & 0xFFF8) == 0x48E0 {
        return Some(DecodedMemOp::MovemLongPreDec {
            base: (opcode & 7) as u8,
        });
    }
    if (opcode & 0xFFF8) == 0x4CD8 {
        return Some(DecodedMemOp::MovemLongPostInc {
            base: (opcode & 7) as u8,
        });
    }

    // LEA: 0100 rrr1 11mm mrrr
    if (opcode & 0xF1C0) == 0x41C0 {
        let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
        if !ea.is_control() {
            return None;
        }
        return Some(DecodedMemOp::Lea {
            reg: ((opcode >> 9) & 7) as u8,
            ea,
        });
    }

    // PEA: 0100 1000 01mm mrrr
    if (opcode & 0xFFC0) == 0x4840 {
        let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
        if !ea.is_control() {
            return None;
        }
        return Some(DecodedMemOp::Pea { ea });
    }

    // JSR: 0100 1110 10mm mrrr / JMP: 0100 1110 11mm mrrr
    if (opcode & 0xFFC0) == 0x4E80 || (opcode & 0xFFC0) == 0x4EC0 {
        let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
        if !ea.is_control() {
            return None;
        }
        return Some(if (opcode & 0x0040) != 0 {
            DecodedMemOp::Jmp { ea }
        } else {
            DecodedMemOp::Jsr { ea }
        });
    }

    // TST / CLR / NEG / NOT: 0100 xxxx ssmm mrrr
    let size = decode_size_00((opcode >> 6) & 3)?;
    let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
    // Memory forms only: register forms are already simple ops, and the
    // odd corners (TST.W An on 68020+, TST #imm, …) keep exact dispatch.
    if !ea.is_memory() || !ea.is_data_alterable() {
        // TST on 68020+ additionally allows PC-relative and immediate
        // sources; keep those on the dispatch path.
        return None;
    }
    let _ = cpu_type;
    match opcode & 0xFF00 {
        0x4A00 => Some(DecodedMemOp::Tst { size, ea }),
        0x4200 => Some(DecodedMemOp::Clr { size, ea }),
        0x4400 => Some(DecodedMemOp::Neg { size, ea }),
        0x4600 => Some(DecodedMemOp::Not { size, ea }),
        _ => None,
    }
}

fn decode_group_5(opcode: u16) -> Option<DecodedMemOp> {
    let size_bits = (opcode >> 6) & 3;
    if size_bits == 3 {
        // DBcc: 0101 cccc 1100 1rrr (Scc with mode 1)
        if ((opcode >> 3) & 7) == 1 {
            return Some(DecodedMemOp::Dbcc {
                condition: ((opcode >> 8) & 0xF) as u8,
                reg: (opcode & 7) as u8,
            });
        }
        return None;
    }

    // ADDQ/SUBQ #q,<ea> with a memory EA (register forms are simple ops).
    let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;
    if !ea.is_memory() || !ea.is_data_alterable() {
        return None;
    }
    let size = decode_size_00(size_bits)?;
    let data = ((opcode >> 9) & 7) as u32;
    Some(DecodedMemOp::AddqSubq {
        data: if data == 0 { 8 } else { data },
        size,
        ea,
        is_sub: (opcode & 0x100) != 0,
    })
}

fn decode_group_6(opcode: u16) -> Option<DecodedMemOp> {
    let condition = ((opcode >> 8) & 0xF) as u8;
    let displacement = (opcode & 0xFF) as u8;
    if condition == 1 {
        // BSR — byte or word displacement (long form falls back).
        return match displacement {
            0 => Some(DecodedMemOp::Bsr {
                displacement: 0, // read from the extension word at exec time
                length: 4,
            }),
            0xFF => None,
            d => Some(DecodedMemOp::Bsr {
                displacement: d as i8 as i32,
                length: 2,
            }),
        };
    }
    // Word-displacement Bcc/BRA; short forms are simple ops, long falls back.
    if displacement == 0 {
        return Some(DecodedMemOp::BranchWord { condition });
    }
    None
}

fn decode_alu(opcode: u16) -> Option<DecodedMemOp> {
    let group = (opcode >> 12) & 0xF;
    let reg = ((opcode >> 9) & 7) as u8;
    let op_mode = (opcode >> 6) & 7;
    let ea = FastEa::decode((opcode >> 3) & 7, opcode & 7)?;

    // ADDA/SUBA/CMPA: op_mode 3 (word) / 7 (long).
    if op_mode == 3 || op_mode == 7 {
        let op = match group {
            0x9 => AddrOp::Suba,
            0xB => AddrOp::Cmpa,
            0xD => AddrOp::Adda,
            _ => return None,
        };
        return Some(DecodedMemOp::AluAddr {
            op,
            size: if op_mode == 3 { Size::Word } else { Size::Long },
            src: ea,
            dst: reg,
        });
    }

    let size = decode_size_00(op_mode & 3)?;
    let to_ea = (op_mode & 4) != 0;

    if !to_ea {
        // <ea> op Dn → Dn. Register-direct sources are simple ops already,
        // but immediate/memory sources come through here.
        let op = match group {
            0x8 => BinaryOp::Or,
            0x9 => BinaryOp::Sub,
            0xB => BinaryOp::Cmp,
            0xC => BinaryOp::And,
            0xD => BinaryOp::Add,
            _ => return None,
        };
        // An source is only legal for word/long ADD/SUB/CMP.
        if matches!(ea, FastEa::AddrReg(_))
            && (size == Size::Byte || matches!(op, BinaryOp::Or | BinaryOp::And))
        {
            return None;
        }
        return Some(DecodedMemOp::AluToReg {
            op,
            size,
            src: ea,
            dst: reg,
        });
    }

    // Dn op <ea> → <ea> forms, plus CMPM and EOR.
    match group {
        0xB => {
            // EOR Dn,<ea> — mode 1 here is CMPM (Ay)+,(Ax)+.
            if let FastEa::AddrReg(src) = ea {
                return Some(DecodedMemOp::CmpM {
                    size,
                    src,
                    dst: reg,
                });
            }
            if matches!(ea, FastEa::DataReg(_)) {
                // EOR Dn,Dm is a simple op.
                return None;
            }
            if !ea.is_data_alterable() || !ea.is_memory() {
                return None;
            }
            Some(DecodedMemOp::AluToMem {
                op: BinaryOp::Eor,
                size,
                src: reg,
                dst: ea,
            })
        }
        0x8 | 0x9 | 0xC | 0xD => {
            // OR/SUB/AND/ADD Dn,<ea>: memory-alterable EAs only (mode 0/1
            // here encode SBCD/ABCD/EXG and friends).
            if !ea.is_memory() || !ea.is_data_alterable() {
                return None;
            }
            let op = match group {
                0x8 => BinaryOp::Or,
                0x9 => BinaryOp::Sub,
                0xC => BinaryOp::And,
                0xD => BinaryOp::Add,
                _ => unreachable!(),
            };
            Some(DecodedMemOp::AluToMem {
                op,
                size,
                src: reg,
                dst: ea,
            })
        }
        _ => None,
    }
}

// ============================================================================
// Execute
// ============================================================================

/// The fastmem window, copied out of the `CpuCore` scratch fields.
/// `TRANSLATED` while the PMMU is enabled: guest addresses are logical then,
/// and each access is translated first (`CpuCore::translated_window_off`).
/// It is a type parameter so the physical instantiation of the executor
/// compiles exactly as it did before translation existed.
#[derive(Clone, Copy)]
struct Win<const TRANSLATED: bool>;

impl<const TRANSLATED: bool> Win<TRANSLATED> {
    #[inline]
    fn from_cpu(cpu: &CpuCore) -> Option<Self> {
        debug_assert_eq!(TRANSLATED, cpu.fm_mmu.enabled());
        (cpu.fm_len != 0).then_some(Win)
    }

    // Operands carry prevalidated host addresses, never offsets into a
    // different allocation. No borrow of CpuCore survives register updates.
    #[inline(always)]
    fn data(self, cpu: &CpuCore, addr: u32, bytes: u32, write: bool) -> Option<usize> {
        let off = if TRANSLATED {
            cpu.translated_window_off(addr, bytes, write, false)
        } else {
            cpu.physical_window_off(addr, bytes, write)
        }?;
        Some(cpu.window_host_ptr(off) as usize)
    }

    #[inline(always)]
    fn code(self, cpu: &CpuCore, addr: u32) -> Option<usize> {
        if TRANSLATED {
            return translated_code(cpu, addr);
        }
        cpu.physical_window_off(addr, 2, false)
            .map(|off| cpu.window_host_ptr(off) as usize)
    }

    #[inline]
    fn read(self, off: usize, size: Size) -> u32 {
        unsafe {
            let p = off as *mut u8;
            match size {
                Size::Byte => *p as u32,
                Size::Word => u16::from_be_bytes([*p, *p.add(1)]) as u32,
                Size::Long => u32::from_be_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]),
            }
        }
    }

    #[inline]
    fn write(self, off: usize, size: Size, value: u32) {
        unsafe {
            let p = off as *mut u8;
            match size {
                Size::Byte => *p = value as u8,
                Size::Word => {
                    let b = (value as u16).to_be_bytes();
                    *p = b[0];
                    *p.add(1) = b[1];
                }
                Size::Long => {
                    let b = value.to_be_bytes();
                    *p = b[0];
                    *p.add(1) = b[1];
                    *p.add(2) = b[2];
                    *p.add(3) = b[3];
                }
            }
        }
    }
}

/// [`Win::code`] with the PMMU enabled: usually the page the decoded loop's
/// opcode fetch just translated.
#[inline(never)]
fn translated_code(cpu: &CpuCore, addr: u32) -> Option<usize> {
    cpu.remembered_code_off(addr)
        .or_else(|| cpu.translated_window_off(addr, 2, false, true))
        .map(|off| cpu.window_host_ptr(off) as usize)
}

/// Deferred post-increment/pre-decrement address-register updates. Two
/// slots cover the worst case (CMPM, mem-to-mem MOVE).
#[derive(Clone, Copy, Default)]
struct Pending {
    slots: [(u8, u32); 2],
    count: u8,
}

impl Pending {
    #[inline(always)]
    fn a(&self, cpu: &CpuCore, reg: u8) -> u32 {
        // Later pendings shadow earlier ones (e.g. CMPM (A0)+,(A0)+).
        for i in (0..self.count as usize).rev() {
            if self.slots[i].0 == reg {
                return self.slots[i].1;
            }
        }
        cpu.dar[8 + reg as usize]
    }

    #[inline(always)]
    fn push(&mut self, reg: u8, value: u32) {
        self.slots[self.count as usize] = (reg, value);
        self.count += 1;
    }

    #[inline(always)]
    fn commit(&self, cpu: &mut CpuCore) {
        for i in 0..self.count as usize {
            cpu.dar[8 + self.slots[i].0 as usize] = self.slots[i].1;
        }
    }
}

/// Per-instruction EA/extension-word cursor. `next_ext` is the guest
/// address of the next unread extension word (starts at PC, which the
/// fetch already advanced past the opcode).
struct Ctx {
    next_ext: u32,
    pending: Pending,
    aligned_only: bool,
}

/// A resolved operand location.
enum Loc {
    /// dar index (0-7 data, 8-15 address)
    Reg(usize),
    /// window offset
    Mem(usize),
    Imm(u32),
}

/// Specialized MOVE for the register / register-indirect EA forms
/// (`Dn`/`An`/`(An)`/`(An)+`/`-(An)`) — no extension words, so no
/// `Ctx`/`Pending` bookkeeping is needed and register adjustments can be
/// staged in locals. Returns `None` when either EA has another form (the
/// generic path handles it), `Some(false)` on window/alignment misses
/// (full dispatch takes over with nothing committed), `Some(true)` when
/// fully executed.
#[inline]
fn fast_move<const T: bool>(
    cpu: &mut CpuCore,
    win: Win<T>,
    size: Size,
    src: FastEa,
    dst: FastEa,
) -> Option<bool> {
    // (addr-register index, staged new value) for post-inc/pre-dec.
    let mut src_adj: Option<(usize, u32)> = None;
    let aligned_only = cpu.is_pre_68020;

    // Window offset for an access at `raw`, or an alignment/range miss.
    let locate = |cpu: &CpuCore, raw: u32, write: bool| -> Result<usize, ()> {
        if aligned_only && size != Size::Byte && (raw & 1) != 0 {
            return Err(());
        }
        win.data(cpu, cpu.address(raw), size.bytes(), write)
            .ok_or(())
    };

    let value = match src {
        FastEa::DataReg(r) => cpu.dar[r as usize] & size.mask(),
        FastEa::AddrReg(r) => cpu.dar[8 + r as usize] & size.mask(),
        FastEa::AnInd(r) => {
            let Ok(off) = locate(cpu, cpu.dar[8 + r as usize], false) else {
                return Some(false);
            };
            win.read(off, size)
        }
        FastEa::AnPostInc(r) => {
            let a = cpu.dar[8 + r as usize];
            let Ok(off) = locate(cpu, a, false) else {
                return Some(false);
            };
            src_adj = Some((8 + r as usize, a.wrapping_add(ea_step(size, r))));
            win.read(off, size)
        }
        FastEa::AnPreDec(r) => {
            let a = cpu.dar[8 + r as usize].wrapping_sub(ea_step(size, r));
            let Ok(off) = locate(cpu, a, false) else {
                return Some(false);
            };
            src_adj = Some((8 + r as usize, a));
            win.read(off, size)
        }
        _ => return None,
    };

    // An address-register base must see a same-register source adjustment
    // (e.g. `MOVE.L (A0)+,(A0)+`).
    let addr_base = |cpu: &CpuCore, r: u8| -> u32 {
        match src_adj {
            Some((idx, val)) if idx == 8 + r as usize => val,
            _ => cpu.dar[8 + r as usize],
        }
    };

    match dst {
        FastEa::DataReg(r) => {
            if let Some((idx, val)) = src_adj {
                cpu.dar[idx] = val;
            }
            let mask = size.mask();
            let r = r as usize;
            cpu.dar[r] = (cpu.dar[r] & !mask) | value;
            cpu.set_logic_flags(value, size);
        }
        FastEa::AddrReg(r) => {
            // MOVEA: sign-extend word, no flags.
            if let Some((idx, val)) = src_adj {
                cpu.dar[idx] = val;
            }
            cpu.dar[8 + r as usize] = if size == Size::Word {
                value as u16 as i16 as i32 as u32
            } else {
                value
            };
        }
        FastEa::AnInd(r) => {
            let Ok(off) = locate(cpu, addr_base(cpu, r), true) else {
                return Some(false);
            };
            if let Some((idx, val)) = src_adj {
                cpu.dar[idx] = val;
            }
            win.write(off, size, value);
            cpu.set_logic_flags(value, size);
        }
        FastEa::AnPostInc(r) => {
            let a = addr_base(cpu, r);
            let Ok(off) = locate(cpu, a, true) else {
                return Some(false);
            };
            if let Some((idx, val)) = src_adj {
                cpu.dar[idx] = val;
            }
            cpu.dar[8 + r as usize] = a.wrapping_add(ea_step(size, r));
            win.write(off, size, value);
            cpu.set_logic_flags(value, size);
        }
        FastEa::AnPreDec(r) => {
            let a = addr_base(cpu, r).wrapping_sub(ea_step(size, r));
            let Ok(off) = locate(cpu, a, true) else {
                return Some(false);
            };
            if let Some((idx, val)) = src_adj {
                cpu.dar[idx] = val;
            }
            cpu.dar[8 + r as usize] = a;
            win.write(off, size, value);
            cpu.set_logic_flags(value, size);
        }
        _ => return None,
    }
    Some(true)
}

/// Data-register-only MOVEM.W (An)+. The extension word and complete source
/// span are checked before any destination register or An is changed.
#[inline]
fn fast_movem_word_postinc<const T: bool>(cpu: &mut CpuCore, win: Win<T>, base: u8) -> bool {
    let Some(mask_off) = win.code(cpu, cpu.address(cpu.pc)) else {
        return false;
    };
    let mask = win.read(mask_off, Size::Word) as u16;
    if mask == 0 || (mask & 0xFF00) != 0 {
        return false;
    }
    let data_mask = mask as u8;
    let bytes = data_mask.count_ones() * 2;
    let raw = cpu.dar[8 + base as usize];
    if cpu.is_pre_68020 && (raw & 1) != 0 {
        return false;
    }
    let masked = cpu.address(raw);
    if masked as u64 + bytes as u64 > cpu.address_mask as u64 + 1 {
        return false;
    }
    let Some(mut off) = win.data(cpu, masked, bytes, false) else {
        return false;
    };

    for reg in 0..8 {
        if (data_mask & (1 << reg)) == 0 {
            continue;
        }
        cpu.dar[reg] = win.read(off, Size::Word) as u16 as i16 as i32 as u32;
        off += 2;
    }
    cpu.dar[8 + base as usize] = raw.wrapping_add(bytes);
    cpu.pc = cpu.pc.wrapping_add(2);
    true
}

/// BTST/BCHG/BCLR/BSET `#imm, Dn` — long-width register bit ops. Only Z is
/// affected (from the pre-modification value), matching the slow path.
fn fast_bit_imm_dreg<const T: bool>(cpu: &mut CpuCore, win: Win<T>, op: BitOp, reg: u8) -> bool {
    let Some(ext_off) = win.code(cpu, cpu.address(cpu.pc)) else {
        return false;
    };
    let bit = win.read(ext_off, Size::Word) & 31;
    let mask = 1u32 << bit;
    let reg = reg as usize;
    let value = cpu.dar[reg];
    cpu.not_z_flag = u32::from(value & mask != 0);
    match op {
        BitOp::Test => {}
        BitOp::Set => cpu.dar[reg] = value | mask,
        BitOp::Clear => cpu.dar[reg] = value & !mask,
        BitOp::Change => cpu.dar[reg] = value ^ mask,
    }
    cpu.pc = cpu.pc.wrapping_add(2);
    true
}

/// LINK An,#disp (word form). `LINK A7` has version-dependent semantics and
/// bails to the slow path.
fn fast_link<const T: bool>(cpu: &mut CpuCore, win: Win<T>, reg: u8) -> bool {
    if reg == 7 {
        return false;
    }
    let Some(ext_off) = win.code(cpu, cpu.address(cpu.pc)) else {
        return false;
    };
    let sp = cpu.dar[15].wrapping_sub(4);
    if cpu.is_pre_68020 && (sp & 1) != 0 {
        return false;
    }
    let Some(off) = win.data(cpu, cpu.address(sp), 4, true) else {
        return false;
    };
    let disp = win.read(ext_off, Size::Word) as u16 as i16 as i32;
    win.write(off, Size::Long, cpu.dar[8 + reg as usize]);
    cpu.dar[8 + reg as usize] = sp;
    cpu.dar[15] = (sp as i32).wrapping_add(disp) as u32;
    cpu.pc = cpu.pc.wrapping_add(2);
    true
}

/// UNLK An. `UNLK A7` bails to the slow path.
fn fast_unlk<const T: bool>(cpu: &mut CpuCore, win: Win<T>, reg: u8) -> bool {
    if reg == 7 {
        return false;
    }
    let sp = cpu.dar[8 + reg as usize];
    if cpu.is_pre_68020 && (sp & 1) != 0 {
        return false;
    }
    let Some(off) = win.data(cpu, cpu.address(sp), 4, false) else {
        return false;
    };
    cpu.dar[8 + reg as usize] = win.read(off, Size::Long);
    cpu.dar[15] = sp.wrapping_add(4);
    // UNLK has no extension word: the decoded loop already advanced the PC
    // past the opcode, so there is nothing further to consume.
    true
}

/// MOVEM.L list,-(An). Predecrement mask order: bit 0 = A7 … bit 15 = D0.
/// Bails when the base register is in the list (version-dependent stored
/// value) or anything misses the window.
fn fast_movem_long_predec<const T: bool>(cpu: &mut CpuCore, win: Win<T>, base: u8) -> bool {
    let Some(mask_off) = win.code(cpu, cpu.address(cpu.pc)) else {
        return false;
    };
    let mask = win.read(mask_off, Size::Word) as u16;
    if mask == 0 || (mask & (1 << (7 - base))) != 0 {
        return false;
    }
    let bytes = mask.count_ones() * 4;
    let raw = cpu.dar[8 + base as usize];
    if cpu.is_pre_68020 && (raw & 1) != 0 {
        return false;
    }
    let start = raw.wrapping_sub(bytes);
    let Some(mut off) = win.data(cpu, cpu.address(start), bytes, true) else {
        return false;
    };
    // Ascending memory order for predec is D0..D7, A0..A6 (mask bit 15-r for
    // register index r).
    for r in 0..16 {
        if (mask & (1 << (15 - r))) == 0 {
            continue;
        }
        win.write(off, Size::Long, cpu.dar[r]);
        off += 4;
    }
    cpu.dar[8 + base as usize] = start;
    cpu.pc = cpu.pc.wrapping_add(2);
    true
}

/// MOVEM.L (An)+,list. Postincrement mask order: bit 0 = D0 … bit 15 = A7.
/// Bails when the base register is in the list.
fn fast_movem_long_postinc<const T: bool>(cpu: &mut CpuCore, win: Win<T>, base: u8) -> bool {
    let Some(mask_off) = win.code(cpu, cpu.address(cpu.pc)) else {
        return false;
    };
    let mask = win.read(mask_off, Size::Word) as u16;
    if mask == 0 || (mask & (1 << (8 + base))) != 0 {
        return false;
    }
    let bytes = mask.count_ones() * 4;
    let raw = cpu.dar[8 + base as usize];
    if cpu.is_pre_68020 && (raw & 1) != 0 {
        return false;
    }
    let Some(mut off) = win.data(cpu, cpu.address(raw), bytes, false) else {
        return false;
    };
    for r in 0..16 {
        if (mask & (1 << r)) == 0 {
            continue;
        }
        cpu.dar[r] = win.read(off, Size::Long);
        off += 4;
    }
    cpu.dar[8 + base as usize] = raw.wrapping_add(bytes);
    cpu.pc = cpu.pc.wrapping_add(2);
    true
}

/// Specialized execution for d16(An) data accesses. Keeping the extension
/// cursor, resolved offset, and value in locals avoids the generic
/// Ctx/Pending/Loc state machine while retaining its all-checks-before-commit
/// fallback contract.
#[inline]
fn fast_an_disp<const T: bool>(cpu: &mut CpuCore, win: Win<T>, op: DecodedMemOp) -> Option<bool> {
    let read_word = |cpu: &CpuCore, pc: u32| -> Result<u32, ()> {
        let off = win.code(cpu, cpu.address(pc)).ok_or(())?;
        Ok(win.read(off, Size::Word))
    };
    let locate = |cpu: &CpuCore, reg: u8, disp: u32, size: Size, write: bool| {
        let raw = (cpu.dar[8 + reg as usize] as i32).wrapping_add(disp as u16 as i16 as i32) as u32;
        if cpu.is_pre_68020 && size != Size::Byte && (raw & 1) != 0 {
            return Err(());
        }
        win.data(cpu, cpu.address(raw), size.bytes(), write)
            .ok_or(())
    };

    match op {
        DecodedMemOp::Move {
            size,
            src: FastEa::AnDisp(src),
            dst: FastEa::DataReg(dst),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, src, ext, size, false) else {
                return Some(false);
            };
            let value = win.read(off, size);
            let mask = size.mask();
            let dst = dst as usize;
            cpu.dar[dst] = (cpu.dar[dst] & !mask) | value;
            cpu.set_logic_flags(value, size);
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::Move {
            size,
            src: FastEa::AnDisp(src),
            dst: FastEa::AddrReg(dst),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, src, ext, size, false) else {
                return Some(false);
            };
            let value = win.read(off, size);
            cpu.dar[8 + dst as usize] = if size == Size::Word {
                value as u16 as i16 as i32 as u32
            } else {
                value
            };
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::Move {
            size,
            src: FastEa::DataReg(src),
            dst: FastEa::AnDisp(dst),
        }
        | DecodedMemOp::Move {
            size,
            src: FastEa::AddrReg(src),
            dst: FastEa::AnDisp(dst),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, dst, ext, size, true) else {
                return Some(false);
            };
            let src_index = if matches!(
                op,
                DecodedMemOp::Move {
                    src: FastEa::DataReg(_),
                    ..
                }
            ) {
                src as usize
            } else {
                8 + src as usize
            };
            let value = cpu.dar[src_index] & size.mask();
            win.write(off, size, value);
            cpu.set_logic_flags(value, size);
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::Move {
            size,
            src: FastEa::AnDisp(src),
            dst: FastEa::AnPreDec(dst),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(src_off) = locate(cpu, src, ext, size, false) else {
                return Some(false);
            };
            let dst_addr = cpu.dar[8 + dst as usize].wrapping_sub(ea_step(size, dst));
            if cpu.is_pre_68020 && size != Size::Byte && (dst_addr & 1) != 0 {
                return Some(false);
            }
            let Some(dst_off) = win.data(cpu, cpu.address(dst_addr), size.bytes(), true) else {
                return Some(false);
            };
            let value = win.read(src_off, size);
            cpu.dar[8 + dst as usize] = dst_addr;
            win.write(dst_off, size, value);
            cpu.set_logic_flags(value, size);
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::Move {
            size,
            src: FastEa::AnDisp(src),
            dst: FastEa::AnDisp(dst),
        } => {
            let Ok(src_ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(dst_ext) = read_word(cpu, cpu.pc.wrapping_add(2)) else {
                return Some(false);
            };
            let Ok(src_off) = locate(cpu, src, src_ext, size, false) else {
                return Some(false);
            };
            let Ok(dst_off) = locate(cpu, dst, dst_ext, size, true) else {
                return Some(false);
            };
            let value = win.read(src_off, size);
            win.write(dst_off, size, value);
            cpu.set_logic_flags(value, size);
            cpu.pc = cpu.pc.wrapping_add(4);
            Some(true)
        }
        DecodedMemOp::Tst {
            size,
            ea: FastEa::AnDisp(reg),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, reg, ext, size, false) else {
                return Some(false);
            };
            cpu.set_logic_flags(win.read(off, size), size);
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::Clr {
            size,
            ea: FastEa::AnDisp(reg),
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, reg, ext, size, true) else {
                return Some(false);
            };
            win.write(off, size, 0);
            cpu.n_flag = 0;
            cpu.not_z_flag = 0;
            cpu.v_flag = 0;
            cpu.c_flag = 0;
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::AddqSubq {
            data,
            size,
            ea: FastEa::AnDisp(reg),
            is_sub,
        } => {
            let Ok(ext) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, reg, ext, size, true) else {
                return Some(false);
            };
            let dst = win.read(off, size);
            let result = if is_sub {
                dst.wrapping_sub(data)
            } else {
                dst.wrapping_add(data)
            };
            win.write(off, size, result & size.mask());
            if is_sub {
                cpu.set_sub_flags(data, dst, result, size);
            } else {
                cpu.set_add_flags(data, dst, result, size);
            }
            cpu.pc = cpu.pc.wrapping_add(2);
            Some(true)
        }
        DecodedMemOp::BitMem {
            op,
            bit: BitSource::Imm,
            ea: FastEa::AnDisp(reg),
        } => {
            let Ok(bit) = read_word(cpu, cpu.pc) else {
                return Some(false);
            };
            let Ok(ext) = read_word(cpu, cpu.pc.wrapping_add(2)) else {
                return Some(false);
            };
            let Ok(off) = locate(cpu, reg, ext, Size::Byte, op != BitOp::Test) else {
                return Some(false);
            };
            let bit = bit & 7;
            let mask = 1 << bit;
            let value = win.read(off, Size::Byte);
            cpu.not_z_flag = u32::from(value & mask != 0);
            match op {
                BitOp::Test => {}
                BitOp::Set => win.write(off, Size::Byte, value | mask),
                BitOp::Clear => win.write(off, Size::Byte, value & !mask),
                BitOp::Change => win.write(off, Size::Byte, value ^ mask),
            }
            cpu.pc = cpu.pc.wrapping_add(4);
            Some(true)
        }
        _ => None,
    }
}

/// Specialized DBcc/BSR/RTS/Bcc.W execution: at most one extension word,
/// no EA resolution, so the `Ctx` plumbing is skipped. Mirrors the generic
/// arms exactly (which remain the single source of truth for the other
/// ops). Returns `false` on any window/alignment miss so full dispatch
/// takes over with nothing committed.
#[inline]
fn fast_flow<const T: bool>(cpu: &mut CpuCore, win: Win<T>, op: DecodedMemOp) -> bool {
    // One extension word at `pc` (the word after the opcode).
    let read_ext = |cpu: &CpuCore| -> Option<u32> {
        let off = win.code(cpu, cpu.address(cpu.pc))?;
        Some(win.read(off, Size::Word))
    };

    match op {
        DecodedMemOp::Dbcc { condition, reg } => {
            let Some(disp) = read_ext(cpu) else {
                return false;
            };
            if !cpu.test_condition(condition) {
                let reg = reg as usize;
                let counter = (cpu.dar[reg] as u16).wrapping_sub(1);
                cpu.dar[reg] = (cpu.dar[reg] & 0xFFFF_0000) | counter as u32;
                if counter != 0xFFFF {
                    // Displacement is relative to the displacement word.
                    cpu.pc = (cpu.pc as i32).wrapping_add(disp as u16 as i16 as i32) as u32;
                    return true;
                }
            }
            cpu.pc = cpu.pc.wrapping_add(2);
            true
        }
        DecodedMemOp::Rts => {
            let sp = cpu.dar[15];
            if cpu.is_pre_68020 && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, false) else {
                return false;
            };
            let ret = win.read(off, Size::Long);
            cpu.dar[15] = sp.wrapping_add(4);
            cpu.change_of_flow = true;
            cpu.pc = ret;
            true
        }
        DecodedMemOp::Bsr {
            displacement,
            length,
        } => {
            let base = cpu.pc;
            let disp = if length == 4 {
                let Some(v) = read_ext(cpu) else {
                    return false;
                };
                v as u16 as i16 as i32
            } else {
                displacement
            };
            let ret = base.wrapping_add(length - 2);
            let sp = cpu.dar[15].wrapping_sub(4);
            if cpu.is_pre_68020 && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, true) else {
                return false;
            };
            win.write(off, Size::Long, ret);
            cpu.dar[15] = sp;
            cpu.change_of_flow = true;
            cpu.pc = (base as i32).wrapping_add(disp) as u32;
            true
        }
        DecodedMemOp::BranchWord { condition } => {
            let base = cpu.pc;
            let Some(disp) = read_ext(cpu) else {
                return false;
            };
            if condition == 0 || cpu.test_condition(condition) {
                cpu.change_of_flow = true;
                cpu.pc = (base as i32).wrapping_add(disp as u16 as i16 as i32) as u32;
            } else {
                cpu.pc = base.wrapping_add(2);
            }
            true
        }
        _ => unreachable!("fast_flow only handles flow-control ops"),
    }
}

fn ea_step(size: Size, reg: u8) -> u32 {
    // Byte accesses through (A7)+ / -(A7) keep the stack pointer even.
    if size == Size::Byte && reg == 7 {
        2
    } else {
        size.bytes()
    }
}

#[inline(always)]
fn read_ext_word<const T: bool>(cpu: &CpuCore, win: Win<T>, ctx: &mut Ctx) -> Option<u32> {
    let addr = cpu.address(ctx.next_ext);
    let off = win.code(cpu, addr)?;
    ctx.next_ext = ctx.next_ext.wrapping_add(2);
    Some(win.read(off, Size::Word))
}

/// Brief-format index computation, mirroring `CpuCore::compute_index`.
/// Returns None for the 68020+ full extension format.
///
/// The index register is read through `pending` so an earlier EA's
/// post-increment/pre-decrement of the same address register is visible
/// (e.g. `MOVE.L -(A0),(d8,A4,A0.W)` must index with the decremented A0).
#[inline]
fn brief_index(cpu: &CpuCore, pending: &Pending, base: u32, ext: u32) -> Option<u32> {
    let is_020_plus = !is_pre_68020(cpu.cpu_type);
    if (ext & 0x0100) != 0 && is_020_plus {
        return None;
    }
    let d8 = (ext & 0xFF) as u8 as i8 as i32;
    let idx_reg = ((ext >> 12) & 0xF) as usize;
    let idx_is_addr = (ext & 0x8000) != 0;
    let idx_is_long = (ext & 0x0800) != 0;
    let scale = if is_020_plus {
        1i32 << ((ext >> 9) & 0x3)
    } else {
        1i32
    };
    let idx_val = if idx_is_addr {
        pending.a(cpu, (idx_reg & 7) as u8)
    } else {
        cpu.dar[idx_reg & 7]
    };
    let idx_val = if idx_is_long {
        idx_val as i32
    } else {
        (idx_val as i16) as i32
    };
    let idx_val = idx_val.wrapping_mul(scale);
    Some((base as i32).wrapping_add(d8).wrapping_add(idx_val) as u32)
}

/// Resolve one EA to a location, reading extension words and recording
/// pending register updates. `write` asks for write permission on a memory
/// operand the instruction writes (or reads and writes). No CPU state is
/// modified.
#[inline(always)]
fn resolve<const T: bool>(
    cpu: &CpuCore,
    win: Win<T>,
    ea: FastEa,
    size: Size,
    write: bool,
    ctx: &mut Ctx,
) -> Option<Loc> {
    match ea {
        FastEa::DataReg(r) => Some(Loc::Reg(r as usize)),
        FastEa::AddrReg(r) => Some(Loc::Reg(8 + r as usize)),
        FastEa::AnInd(r) => resolve_mem(cpu, win, ctx, ctx.pending.a(cpu, r), size, write),
        FastEa::AnPostInc(r) => {
            let a = ctx.pending.a(cpu, r);
            let loc = resolve_mem(cpu, win, ctx, a, size, write)?;
            ctx.pending.push(r, a.wrapping_add(ea_step(size, r)));
            Some(loc)
        }
        FastEa::AnPreDec(r) => {
            let a = ctx.pending.a(cpu, r).wrapping_sub(ea_step(size, r));
            let loc = resolve_mem(cpu, win, ctx, a, size, write)?;
            ctx.pending.push(r, a);
            Some(loc)
        }
        FastEa::AnDisp(r) => {
            let d = read_ext_word(cpu, win, ctx)? as u16 as i16 as i32;
            let a = (ctx.pending.a(cpu, r) as i32).wrapping_add(d) as u32;
            resolve_mem(cpu, win, ctx, a, size, write)
        }
        FastEa::AnIndex(r) => {
            let ext = read_ext_word(cpu, win, ctx)?;
            let a = brief_index(cpu, &ctx.pending, ctx.pending.a(cpu, r), ext)?;
            resolve_mem(cpu, win, ctx, a, size, write)
        }
        FastEa::AbsW => {
            let a = read_ext_word(cpu, win, ctx)? as u16 as i16 as i32 as u32;
            resolve_mem(cpu, win, ctx, a, size, write)
        }
        FastEa::AbsL => {
            let hi = read_ext_word(cpu, win, ctx)?;
            let lo = read_ext_word(cpu, win, ctx)?;
            resolve_mem(cpu, win, ctx, (hi << 16) | lo, size, write)
        }
        FastEa::PcDisp => {
            // Base is the address of the extension word itself.
            let base = ctx.next_ext;
            let d = read_ext_word(cpu, win, ctx)? as u16 as i16 as i32;
            resolve_mem(
                cpu,
                win,
                ctx,
                (base as i32).wrapping_add(d) as u32,
                size,
                write,
            )
        }
        FastEa::PcIndex => {
            let base = ctx.next_ext;
            let ext = read_ext_word(cpu, win, ctx)?;
            let a = brief_index(cpu, &ctx.pending, base, ext)?;
            resolve_mem(cpu, win, ctx, a, size, write)
        }
        FastEa::Imm => {
            let v = match size {
                Size::Byte => read_ext_word(cpu, win, ctx)? & 0xFF,
                Size::Word => read_ext_word(cpu, win, ctx)?,
                Size::Long => {
                    let hi = read_ext_word(cpu, win, ctx)?;
                    let lo = read_ext_word(cpu, win, ctx)?;
                    (hi << 16) | lo
                }
            };
            Some(Loc::Imm(v))
        }
    }
}

/// The memory location of a `size` operand at `raw` for [`resolve`]. A
/// function rather than a closure so it can be forced inline: `resolve`
/// runs for every generic memory operand.
#[inline(always)]
fn resolve_mem<const T: bool>(
    cpu: &CpuCore,
    win: Win<T>,
    ctx: &Ctx,
    raw: u32,
    size: Size,
    write: bool,
) -> Option<Loc> {
    if ctx.aligned_only && size != Size::Byte && (raw & 1) != 0 {
        return None;
    }
    let addr = cpu.address(raw);
    Some(Loc::Mem(win.data(cpu, addr, size.bytes(), write)?))
}

/// Resolve a control EA (LEA/PEA/JMP/JSR) to a guest address (unmasked).
fn resolve_control_addr<const T: bool>(
    cpu: &CpuCore,
    win: Win<T>,
    ea: FastEa,
    ctx: &mut Ctx,
) -> Option<u32> {
    match ea {
        FastEa::AnInd(r) => Some(cpu.dar[8 + r as usize]),
        FastEa::AnDisp(r) => {
            let d = read_ext_word(cpu, win, ctx)? as u16 as i16 as i32;
            Some((cpu.dar[8 + r as usize] as i32).wrapping_add(d) as u32)
        }
        FastEa::AnIndex(r) => {
            let ext = read_ext_word(cpu, win, ctx)?;
            brief_index(cpu, &ctx.pending, cpu.dar[8 + r as usize], ext)
        }
        FastEa::AbsW => Some(read_ext_word(cpu, win, ctx)? as u16 as i16 as i32 as u32),
        FastEa::AbsL => {
            let hi = read_ext_word(cpu, win, ctx)?;
            let lo = read_ext_word(cpu, win, ctx)?;
            Some((hi << 16) | lo)
        }
        FastEa::PcDisp => {
            let base = ctx.next_ext;
            let d = read_ext_word(cpu, win, ctx)? as u16 as i16 as i32;
            Some((base as i32).wrapping_add(d) as u32)
        }
        FastEa::PcIndex => {
            let base = ctx.next_ext;
            let ext = read_ext_word(cpu, win, ctx)?;
            brief_index(cpu, &ctx.pending, base, ext)
        }
        _ => None,
    }
}

#[inline(always)]
fn load<const T: bool>(cpu: &CpuCore, win: Win<T>, loc: &Loc, size: Size) -> u32 {
    match *loc {
        Loc::Reg(i) => cpu.dar[i] & size.mask(),
        Loc::Mem(off) => win.read(off, size),
        Loc::Imm(v) => v & size.mask(),
    }
}

#[inline(always)]
fn store<const T: bool>(cpu: &mut CpuCore, win: Win<T>, loc: &Loc, size: Size, value: u32) {
    match *loc {
        Loc::Reg(i) => {
            let mask = size.mask();
            cpu.dar[i] = (cpu.dar[i] & !mask) | (value & mask);
        }
        Loc::Mem(off) => win.write(off, size, value),
        Loc::Imm(_) => unreachable!("immediate is never a destination"),
    }
}

/// Execute a decoded memory op against the fastmem window.
///
/// Returns `false` (with zero state changes) when the op must fall back to
/// full dispatch: window misses, pre-68020 misaligned accesses, 68020+
/// full-format extensions, or no window at all.
/// Side-effect-free diagnostic read of a guest word through the attached
/// fastmem window. Returns `None` when no window is attached or the address
/// is outside it. The `AddressBus` is never involved, so callers cannot add
/// host-visible bus transactions.
#[cfg(feature = "trace-profile")]
pub(crate) fn peek_window_word(cpu: &CpuCore, addr: u32) -> Option<u16> {
    if cpu.fm_mmu.enabled() {
        let win = Win::<true>::from_cpu(cpu)?;
        let off = win.code(cpu, cpu.address(addr))?;
        return Some(win.read(off, Size::Word) as u16);
    }
    let win = Win::<false>::from_cpu(cpu)?;
    let off = win.code(cpu, cpu.address(addr))?;
    Some(win.read(off, Size::Word) as u16)
}

/// Execute `op` through the batch window, or return false (nothing
/// committed) for full dispatch to take it. `TRANSLATED` is the window
/// mode, which the caller knows statically: each instance of the decoded
/// batch loop calls its executor directly. Going through one entry that
/// branched on the mode cost the physical loop two taken branches and a
/// tail call per memory op, enough to leave it measurably front-end bound
/// in a whole-program (fat LTO) build.
#[inline(always)]
pub(crate) fn execute_window_mem_op<const TRANSLATED: bool>(
    cpu: &mut CpuCore,
    op: DecodedMemOp,
) -> bool {
    if TRANSLATED {
        execute_translated_mem_op(cpu, op)
    } else {
        execute_physical_mem_op(cpu, op)
    }
}

/// [`execute_window_mem_op`] for callers that learn only at run time
/// whether the window translates.
#[cfg(any(not(feature = "jit"), target_family = "wasm", test))]
pub(crate) fn execute_mem_op(cpu: &mut CpuCore, op: DecodedMemOp) -> bool {
    if cpu.fm_mmu.enabled() {
        execute_translated_mem_op(cpu, op)
    } else {
        execute_physical_mem_op(cpu, op)
    }
}

/// The executor with the PMMU disabled. It compiles as the executor did
/// before translation existed.
fn execute_physical_mem_op(cpu: &mut CpuCore, op: DecodedMemOp) -> bool {
    execute_mem_op_in::<false>(cpu, op)
}

/// The executor through a translated window, kept out of line like the
/// physical one.
#[inline(never)]
fn execute_translated_mem_op(cpu: &mut CpuCore, op: DecodedMemOp) -> bool {
    execute_mem_op_in::<true>(cpu, op)
}

/// The executor body, inlined into both executors so that neither is a
/// wrapper around a shared copy.
#[inline(always)]
fn execute_mem_op_in<const T: bool>(cpu: &mut CpuCore, op: DecodedMemOp) -> bool {
    // These decoded handlers perform raw stores. Until they support tracked
    // writes, preserve ordinary dispatch and its metadata hooks between traces.
    if cpu.fm_write_hook != 0 {
        return false;
    }
    let Some(win) = Win::<T>::from_cpu(cpu) else {
        return false;
    };

    // MOVE between registers and register-indirect memory is by far the
    // hottest mem op (block copies, loads, stores); handle it without the
    // generic Ctx/Pending resolution machinery. `None` means "not this
    // shape", falling through to the generic path below.
    if let DecodedMemOp::Move { size, src, dst } = op
        && let Some(handled) = fast_move(cpu, win, size, src, dst)
    {
        return handled;
    }

    if let DecodedMemOp::MovemWordPostInc { base } = op {
        return fast_movem_word_postinc(cpu, win, base);
    }
    if let DecodedMemOp::BitImmDataReg { op, reg } = op {
        return fast_bit_imm_dreg(cpu, win, op, reg);
    }
    if let DecodedMemOp::Link { reg } = op {
        return fast_link(cpu, win, reg);
    }
    if let DecodedMemOp::Unlk { reg } = op {
        return fast_unlk(cpu, win, reg);
    }
    if let DecodedMemOp::MovemLongPreDec { base } = op {
        return fast_movem_long_predec(cpu, win, base);
    }
    if let DecodedMemOp::MovemLongPostInc { base } = op {
        return fast_movem_long_postinc(cpu, win, base);
    }

    if let Some(handled) = fast_an_disp(cpu, win, op) {
        return handled;
    }

    // Flow-control ops (loop closers, calls, returns) are the other hot
    // class; they need at most one extension word and no EA resolution.
    match op {
        DecodedMemOp::Dbcc { .. }
        | DecodedMemOp::Bsr { .. }
        | DecodedMemOp::Rts
        | DecodedMemOp::BranchWord { .. } => return fast_flow(cpu, win, op),
        _ => {}
    }

    let mut ctx = Ctx {
        next_ext: cpu.pc,
        pending: Pending::default(),
        aligned_only: is_pre_68020(cpu.cpu_type),
    };

    macro_rules! finish_pc {
        () => {
            cpu.pc = ctx.next_ext;
        };
    }

    match op {
        DecodedMemOp::BitImmDataReg { .. }
        | DecodedMemOp::Link { .. }
        | DecodedMemOp::Unlk { .. }
        | DecodedMemOp::MovemLongPreDec { .. }
        | DecodedMemOp::MovemLongPostInc { .. } => false,
        DecodedMemOp::Move { size, src, dst } => {
            let Some(src_loc) = resolve(cpu, win, src, size, false, &mut ctx) else {
                return false;
            };
            let value = load(cpu, win, &src_loc, size);
            if let FastEa::AddrReg(r) = dst {
                // MOVEA: sign-extend word, no flags.
                let value = if size == Size::Word {
                    value as u16 as i16 as i32 as u32
                } else {
                    value
                };
                ctx.pending.commit(cpu);
                cpu.dar[8 + r as usize] = value;
                finish_pc!();
                return true;
            }
            let Some(dst_loc) = resolve(cpu, win, dst, size, true, &mut ctx) else {
                return false;
            };
            ctx.pending.commit(cpu);
            store(cpu, win, &dst_loc, size, value);
            cpu.set_logic_flags(value, size);
            finish_pc!();
            true
        }
        DecodedMemOp::MovemWordPostInc { .. } => {
            unreachable!("MOVEM.W postincrement is handled by its fast path")
        }
        DecodedMemOp::AluToReg { op, size, src, dst } => {
            let Some(src_loc) = resolve(cpu, win, src, size, false, &mut ctx) else {
                return false;
            };
            let src_val = load(cpu, win, &src_loc, size);
            ctx.pending.commit(cpu);
            let dst_val = cpu.dar[dst as usize] & size.mask();
            apply_binary_to_reg(cpu, op, size, src_val, dst_val, dst as usize);
            finish_pc!();
            true
        }
        DecodedMemOp::AluToMem { op, size, src, dst } => {
            let Some(dst_loc) = resolve(cpu, win, dst, size, true, &mut ctx) else {
                return false;
            };
            let dst_val = load(cpu, win, &dst_loc, size);
            let src_val = cpu.dar[src as usize] & size.mask();
            let result = match op {
                BinaryOp::Add => src_val.wrapping_add(dst_val),
                BinaryOp::Sub => dst_val.wrapping_sub(src_val),
                BinaryOp::And => src_val & dst_val,
                BinaryOp::Or => src_val | dst_val,
                BinaryOp::Eor => src_val ^ dst_val,
                BinaryOp::Cmp => unreachable!("CMP has no to-memory form"),
            };
            ctx.pending.commit(cpu);
            store(cpu, win, &dst_loc, size, result & size.mask());
            match op {
                BinaryOp::Add => cpu.set_add_flags(src_val, dst_val, result, size),
                BinaryOp::Sub => cpu.set_sub_flags(src_val, dst_val, result, size),
                _ => cpu.set_logic_flags(result, size),
            }
            finish_pc!();
            true
        }
        DecodedMemOp::AluAddr { op, size, src, dst } => {
            let Some(src_loc) = resolve(cpu, win, src, size, false, &mut ctx) else {
                return false;
            };
            let raw = load(cpu, win, &src_loc, size);
            // Address ALU always operates on the full 32-bit register with
            // a sign-extended word source.
            let src_val = if size == Size::Word {
                raw as u16 as i16 as i32 as u32
            } else {
                raw
            };
            ctx.pending.commit(cpu);
            let dst_i = 8 + dst as usize;
            let dst_val = cpu.dar[dst_i];
            match op {
                AddrOp::Adda => cpu.dar[dst_i] = dst_val.wrapping_add(src_val),
                AddrOp::Suba => cpu.dar[dst_i] = dst_val.wrapping_sub(src_val),
                AddrOp::Cmpa => {
                    let result = dst_val.wrapping_sub(src_val);
                    cpu.set_cmp_flags(src_val, dst_val, result, Size::Long);
                }
            }
            finish_pc!();
            true
        }
        DecodedMemOp::AluImm { op, size, dst } => {
            let imm = {
                let Some(Loc::Imm(v)) = resolve(cpu, win, FastEa::Imm, size, false, &mut ctx)
                else {
                    return false;
                };
                v
            };
            let Some(dst_loc) = resolve(cpu, win, dst, size, op != BinaryOp::Cmp, &mut ctx) else {
                return false;
            };
            let dst_val = load(cpu, win, &dst_loc, size);
            let result = match op {
                BinaryOp::Add => imm.wrapping_add(dst_val),
                BinaryOp::Sub | BinaryOp::Cmp => dst_val.wrapping_sub(imm),
                BinaryOp::And => imm & dst_val,
                BinaryOp::Or => imm | dst_val,
                BinaryOp::Eor => imm ^ dst_val,
            };
            ctx.pending.commit(cpu);
            if op != BinaryOp::Cmp {
                store(cpu, win, &dst_loc, size, result & size.mask());
            }
            match op {
                BinaryOp::Add => cpu.set_add_flags(imm, dst_val, result, size),
                BinaryOp::Sub => cpu.set_sub_flags(imm, dst_val, result, size),
                BinaryOp::Cmp => cpu.set_cmp_flags(imm, dst_val, result, size),
                _ => cpu.set_logic_flags(result, size),
            }
            finish_pc!();
            true
        }
        DecodedMemOp::AddqSubq {
            data,
            size,
            ea,
            is_sub,
        } => {
            let Some(loc) = resolve(cpu, win, ea, size, true, &mut ctx) else {
                return false;
            };
            let dst_val = load(cpu, win, &loc, size);
            let result = if is_sub {
                dst_val.wrapping_sub(data)
            } else {
                dst_val.wrapping_add(data)
            };
            ctx.pending.commit(cpu);
            store(cpu, win, &loc, size, result & size.mask());
            if is_sub {
                cpu.set_sub_flags(data, dst_val, result, size);
            } else {
                cpu.set_add_flags(data, dst_val, result, size);
            }
            finish_pc!();
            true
        }
        DecodedMemOp::Tst { size, ea } => {
            let Some(loc) = resolve(cpu, win, ea, size, false, &mut ctx) else {
                return false;
            };
            let value = load(cpu, win, &loc, size);
            ctx.pending.commit(cpu);
            cpu.set_logic_flags(value, size);
            finish_pc!();
            true
        }
        DecodedMemOp::Clr { size, ea } => {
            let Some(loc) = resolve(cpu, win, ea, size, true, &mut ctx) else {
                return false;
            };
            ctx.pending.commit(cpu);
            store(cpu, win, &loc, size, 0);
            cpu.n_flag = 0;
            cpu.not_z_flag = 0;
            cpu.v_flag = 0;
            cpu.c_flag = 0;
            finish_pc!();
            true
        }
        DecodedMemOp::Neg { size, ea } => {
            let Some(loc) = resolve(cpu, win, ea, size, true, &mut ctx) else {
                return false;
            };
            let src = load(cpu, win, &loc, size);
            let result = 0u32.wrapping_sub(src);
            ctx.pending.commit(cpu);
            store(cpu, win, &loc, size, result & size.mask());
            cpu.set_sub_flags(src, 0, result, size);
            finish_pc!();
            true
        }
        DecodedMemOp::Not { size, ea } => {
            let Some(loc) = resolve(cpu, win, ea, size, true, &mut ctx) else {
                return false;
            };
            let src = load(cpu, win, &loc, size);
            let result = !src & size.mask();
            ctx.pending.commit(cpu);
            store(cpu, win, &loc, size, result);
            cpu.set_logic_flags(result, size);
            finish_pc!();
            true
        }
        DecodedMemOp::BitMem { op, bit, ea } => {
            let bit_num = match bit {
                BitSource::Reg(r) => cpu.dar[r as usize],
                BitSource::Imm => {
                    let Some(v) = read_ext_word(cpu, win, &mut ctx) else {
                        return false;
                    };
                    v
                }
            } & 7;
            let Some(loc) = resolve(cpu, win, ea, Size::Byte, op != BitOp::Test, &mut ctx) else {
                return false;
            };
            let value = load(cpu, win, &loc, Size::Byte);
            ctx.pending.commit(cpu);
            cpu.not_z_flag = if value & (1 << bit_num) != 0 { 1 } else { 0 };
            match op {
                BitOp::Test => {}
                BitOp::Set => store(cpu, win, &loc, Size::Byte, value | (1 << bit_num)),
                BitOp::Clear => store(cpu, win, &loc, Size::Byte, value & !(1 << bit_num)),
                BitOp::Change => store(cpu, win, &loc, Size::Byte, value ^ (1 << bit_num)),
            }
            finish_pc!();
            true
        }
        DecodedMemOp::CmpM { size, src, dst } => {
            let Some(src_loc) = resolve(cpu, win, FastEa::AnPostInc(src), size, false, &mut ctx)
            else {
                return false;
            };
            let src_val = load(cpu, win, &src_loc, size);
            let Some(dst_loc) = resolve(cpu, win, FastEa::AnPostInc(dst), size, false, &mut ctx)
            else {
                return false;
            };
            let dst_val = load(cpu, win, &dst_loc, size);
            ctx.pending.commit(cpu);
            let result = dst_val.wrapping_sub(src_val);
            cpu.set_cmp_flags(src_val, dst_val, result, size);
            finish_pc!();
            true
        }
        DecodedMemOp::Lea { reg, ea } => {
            let Some(addr) = resolve_control_addr(cpu, win, ea, &mut ctx) else {
                return false;
            };
            cpu.dar[8 + reg as usize] = addr;
            finish_pc!();
            true
        }
        DecodedMemOp::Pea { ea } => {
            let Some(addr) = resolve_control_addr(cpu, win, ea, &mut ctx) else {
                return false;
            };
            let sp = cpu.dar[15].wrapping_sub(4);
            if ctx.aligned_only && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, true) else {
                return false;
            };
            win.write(off, Size::Long, addr);
            cpu.dar[15] = sp;
            finish_pc!();
            true
        }
        DecodedMemOp::Jmp { ea } => {
            let Some(addr) = resolve_control_addr(cpu, win, ea, &mut ctx) else {
                return false;
            };
            // The interpreter assigns the EA unmasked; the mask is applied
            // at the next fetch. Mirror that so pc values stay identical.
            cpu.change_of_flow = true;
            cpu.pc = addr;
            true
        }
        DecodedMemOp::Jsr { ea } => {
            let Some(addr) = resolve_control_addr(cpu, win, ea, &mut ctx) else {
                return false;
            };
            let ret = ctx.next_ext;
            let sp = cpu.dar[15].wrapping_sub(4);
            if ctx.aligned_only && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, true) else {
                return false;
            };
            win.write(off, Size::Long, ret);
            cpu.dar[15] = sp;
            cpu.change_of_flow = true;
            cpu.pc = addr;
            true
        }
        DecodedMemOp::Rts => {
            let sp = cpu.dar[15];
            if ctx.aligned_only && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, false) else {
                return false;
            };
            let ret = win.read(off, Size::Long);
            cpu.dar[15] = sp.wrapping_add(4);
            cpu.change_of_flow = true;
            cpu.pc = ret;
            true
        }
        DecodedMemOp::Bsr {
            displacement,
            length,
        } => {
            let base = cpu.pc;
            let disp = if length == 4 {
                let Some(v) = read_ext_word(cpu, win, &mut ctx) else {
                    return false;
                };
                v as u16 as i16 as i32
            } else {
                displacement
            };
            let ret = ctx.next_ext;
            let sp = cpu.dar[15].wrapping_sub(4);
            if ctx.aligned_only && (sp & 1) != 0 {
                return false;
            }
            let Some(off) = win.data(cpu, cpu.address(sp), 4, true) else {
                return false;
            };
            win.write(off, Size::Long, ret);
            cpu.dar[15] = sp;
            cpu.change_of_flow = true;
            cpu.pc = (base as i32).wrapping_add(disp) as u32;
            true
        }
        DecodedMemOp::BranchWord { condition } => {
            let base = cpu.pc;
            let Some(disp) = read_ext_word(cpu, win, &mut ctx) else {
                return false;
            };
            if condition == 0 || cpu.test_condition(condition) {
                cpu.change_of_flow = true;
                cpu.pc = (base as i32).wrapping_add(disp as u16 as i16 as i32) as u32;
            } else {
                cpu.pc = ctx.next_ext;
            }
            true
        }
        DecodedMemOp::Dbcc { condition, reg } => {
            let Some(disp) = read_ext_word(cpu, win, &mut ctx) else {
                return false;
            };
            if !cpu.test_condition(condition) {
                let reg = reg as usize;
                let counter = (cpu.dar[reg] as u16).wrapping_sub(1);
                cpu.dar[reg] = (cpu.dar[reg] & 0xFFFF_0000) | counter as u32;
                if counter != 0xFFFF {
                    // Displacement is relative to the displacement word.
                    cpu.pc = (cpu.pc as i32).wrapping_add(disp as u16 as i16 as i32) as u32;
                    return true;
                }
            }
            cpu.pc = ctx.next_ext;
            true
        }
    }
}

#[inline(always)]
fn apply_binary_to_reg(
    cpu: &mut CpuCore,
    op: BinaryOp,
    size: Size,
    src: u32,
    dst: u32,
    reg: usize,
) {
    let mask = size.mask();
    match op {
        BinaryOp::Add => {
            let result = src.wrapping_add(dst);
            cpu.dar[reg] = (cpu.dar[reg] & !mask) | (result & mask);
            cpu.set_add_flags(src, dst, result, size);
        }
        BinaryOp::Sub => {
            let result = dst.wrapping_sub(src);
            cpu.dar[reg] = (cpu.dar[reg] & !mask) | (result & mask);
            cpu.set_sub_flags(src, dst, result, size);
        }
        BinaryOp::Cmp => {
            let result = dst.wrapping_sub(src);
            cpu.set_cmp_flags(src, dst, result, size);
        }
        BinaryOp::And => {
            let result = src & dst;
            cpu.dar[reg] = (cpu.dar[reg] & !mask) | (result & mask);
            cpu.set_logic_flags(result, size);
        }
        BinaryOp::Or => {
            let result = src | dst;
            cpu.dar[reg] = (cpu.dar[reg] & !mask) | (result & mask);
            cpu.set_logic_flags(result, size);
        }
        BinaryOp::Eor => {
            let result = src ^ dst;
            cpu.dar[reg] = (cpu.dar[reg] & !mask) | (result & mask);
            cpu.set_logic_flags(result, size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_with_window(mem: &mut [u8]) -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu.fm_ptr = mem.as_mut_ptr() as usize;
        cpu.fm_base = 0;
        cpu.fm_len = mem.len() as u32;
        cpu.pc = 0x100;
        cpu.set_a(5, 0x400);
        cpu
    }

    #[test]
    fn fast_btst_imm_dreg_sets_only_z_and_bchg_modifies() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&5u16.to_be_bytes());
        let mut cpu = cpu_with_window(&mut mem);
        cpu.dar[3] = 0x20;
        cpu.set_ccr(0x1F);
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::BitImmDataReg {
                op: BitOp::Test,
                reg: 3
            }
        ));
        assert_eq!(cpu.pc, 0x102);
        assert_ne!(cpu.not_z_flag, 0, "bit 5 of $20 is set -> Z clear");
        assert_eq!(cpu.dar[3], 0x20);
        assert_eq!(cpu.get_ccr() & 0x1B, 0x1B, "X/N/V/C untouched");
        cpu.pc = 0x100;
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::BitImmDataReg {
                op: BitOp::Change,
                reg: 3
            }
        ));
        assert_eq!(cpu.dar[3], 0, "BCHG #5 cleared the only set bit");
        assert_ne!(cpu.not_z_flag, 0, "Z reflects the pre-change value");
    }

    #[test]
    fn fast_link_unlk_roundtrip_builds_and_tears_down_the_frame() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&(-8i16 as u16).to_be_bytes());
        let mut cpu = cpu_with_window(&mut mem);
        cpu.dar[15] = 0x800;
        cpu.dar[8 + 6] = 0xDEAD_BEEF;
        assert!(execute_mem_op(&mut cpu, DecodedMemOp::Link { reg: 6 }));
        assert_eq!(cpu.pc, 0x102);
        assert_eq!(cpu.dar[8 + 6], 0x7FC, "A6 = decremented SP");
        assert_eq!(cpu.dar[15], 0x7FC - 8, "SP moved by the displacement");
        assert_eq!(
            u32::from_be_bytes(mem[0x7FC..0x800].try_into().unwrap()),
            0xDEAD_BEEF,
            "old A6 saved on the stack"
        );
        cpu.pc = 0x200;
        assert!(execute_mem_op(&mut cpu, DecodedMemOp::Unlk { reg: 6 }));
        assert_eq!(cpu.pc, 0x200, "UNLK consumes no extension word");
        assert_eq!(cpu.dar[8 + 6], 0xDEAD_BEEF, "A6 restored");
        assert_eq!(cpu.dar[15], 0x800, "SP restored past the saved slot");
        assert!(
            !execute_mem_op(&mut cpu, DecodedMemOp::Link { reg: 7 }),
            "LINK A7 bails to the slow path"
        );
    }

    #[test]
    fn fast_movem_long_predec_postinc_roundtrip_and_base_in_mask_bails() {
        let mut mem = vec![0u8; 0x1000];
        // push mask at pc: D2,D4,A2 -> predec mask bits (15-r): D2=bit13, D4=bit11, A2=bit5
        mem[0x100..0x102].copy_from_slice(&0x2820u16.to_be_bytes());
        // pop mask at 0x200: postinc bits: D2=bit2, D4=bit4, A2=bit10
        mem[0x200..0x202].copy_from_slice(&0x0414u16.to_be_bytes());
        let mut cpu = cpu_with_window(&mut mem);
        cpu.dar[15] = 0x800;
        cpu.dar[2] = 0x1111_2222;
        cpu.dar[4] = 0x3333_4444;
        cpu.dar[8 + 2] = 0x5555_6666;
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::MovemLongPreDec { base: 7 }
        ));
        assert_eq!(cpu.pc, 0x102);
        assert_eq!(cpu.dar[15], 0x800 - 12);
        cpu.dar[2] = 0;
        cpu.dar[4] = 0;
        cpu.dar[8 + 2] = 0;
        cpu.dar[15] = 0x800 - 12;
        cpu.pc = 0x200;
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::MovemLongPostInc { base: 7 }
        ));
        assert_eq!(cpu.pc, 0x202);
        assert_eq!(cpu.dar[15], 0x800, "SP restored");
        assert_eq!(cpu.dar[2], 0x1111_2222);
        assert_eq!(cpu.dar[4], 0x3333_4444);
        assert_eq!(cpu.dar[8 + 2], 0x5555_6666);
        // base register inside the list bails (A7 postinc bit 15)
        mem_set(&mut cpu, 0x202, 0x8000);
        cpu.pc = 0x202;
        assert!(!execute_mem_op(
            &mut cpu,
            DecodedMemOp::MovemLongPostInc { base: 7 }
        ));
        assert_eq!(cpu.pc, 0x202, "bail leaves the PC untouched");
    }

    fn mem_set(cpu: &mut CpuCore, addr: u32, word: u16) {
        unsafe {
            let p = (cpu.fm_ptr as *mut u8).add(addr as usize);
            let b = word.to_be_bytes();
            *p = b[0];
            *p.add(1) = b[1];
        }
    }

    #[test]
    fn fast_movem_word_postincrement_is_atomic_and_sign_extends() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&0x00FEu16.to_be_bytes());
        let words = [0x8000u16, 1, 0x7FFF, 0xFFFF, 0, 0x1234, 0xABCD];
        for (index, word) in words.into_iter().enumerate() {
            mem[0x200 + index * 2..0x202 + index * 2].copy_from_slice(&word.to_be_bytes());
        }
        let mut cpu = cpu_with_window(&mut mem);
        cpu.set_a(0, 0x200);
        cpu.set_ccr(0x1F);
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::MovemWordPostInc { base: 0 }
        ));
        assert_eq!(cpu.pc, 0x102);
        assert_eq!(cpu.a(0), 0x20E);
        assert_eq!(cpu.d(1), 0xFFFF_8000);
        assert_eq!(cpu.d(2), 1);
        assert_eq!(cpu.d(3), 0x7FFF);
        assert_eq!(cpu.d(4), 0xFFFF_FFFF);
        assert_eq!(cpu.d(7), 0xFFFF_ABCD);
        assert_eq!(cpu.get_ccr(), 0x1F);

        cpu.pc = 0x100;
        cpu.set_a(0, 0x0FFA); // complete seven-word source span is outside RAM
        for reg in 1..8 {
            cpu.set_d(reg, 0xA000_0000 | reg as u32);
        }
        assert!(!execute_mem_op(
            &mut cpu,
            DecodedMemOp::MovemWordPostInc { base: 0 }
        ));
        assert_eq!(cpu.pc, 0x100);
        assert_eq!(cpu.a(0), 0x0FFA);
        for reg in 1..8 {
            assert_eq!(cpu.d(reg), 0xA000_0000 | reg as u32);
        }
    }

    #[test]
    fn fast_an_disp_tst_and_clr_advance_extension_cursor() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&0x0012u16.to_be_bytes());
        mem[0x412] = 0x80;
        let mut cpu = cpu_with_window(&mut mem);

        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::Tst {
                size: Size::Byte,
                ea: FastEa::AnDisp(5),
            }
        ));
        assert_eq!(cpu.pc, 0x102);
        assert_ne!(cpu.n_flag, 0);
        assert_ne!(cpu.not_z_flag, 0);

        cpu.pc = 0x100;
        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::Clr {
                size: Size::Byte,
                ea: FastEa::AnDisp(5),
            }
        ));
        assert_eq!(mem[0x412], 0);
        assert_eq!(cpu.pc, 0x102);
        assert_eq!(cpu.not_z_flag, 0);
    }

    #[test]
    fn fast_an_disp_immediate_bit_uses_both_extension_words() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&3u16.to_be_bytes());
        mem[0x102..0x104].copy_from_slice(&0x0012u16.to_be_bytes());
        mem[0x412] = 0b0000_1000;
        let mut cpu = cpu_with_window(&mut mem);

        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::BitMem {
                op: BitOp::Test,
                bit: BitSource::Imm,
                ea: FastEa::AnDisp(5),
            }
        ));
        assert_eq!(cpu.pc, 0x104);
        assert_eq!(cpu.not_z_flag, 1);
        assert_eq!(mem[0x412], 0b0000_1000);
    }

    #[test]
    fn fast_an_disp_move_checks_both_operands_before_commit() {
        let mut mem = vec![0u8; 0x1000];
        mem[0x100..0x102].copy_from_slice(&0x0012u16.to_be_bytes());
        mem[0x412..0x416].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        let mut cpu = cpu_with_window(&mut mem);
        cpu.set_a(7, 0x800);

        assert!(execute_mem_op(
            &mut cpu,
            DecodedMemOp::Move {
                size: Size::Long,
                src: FastEa::AnDisp(5),
                dst: FastEa::AnPreDec(7),
            }
        ));
        assert_eq!(cpu.a(7), 0x7FC);
        assert_eq!(&mem[0x7FC..0x800], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(cpu.pc, 0x102);

        cpu.pc = 0x100;
        cpu.set_a(7, 2);
        let old_flags = (cpu.n_flag, cpu.not_z_flag, cpu.v_flag, cpu.c_flag);
        assert!(!execute_mem_op(
            &mut cpu,
            DecodedMemOp::Move {
                size: Size::Long,
                src: FastEa::AnDisp(5),
                dst: FastEa::AnPreDec(7),
            }
        ));
        assert_eq!(cpu.a(7), 2);
        assert_eq!(cpu.pc, 0x100);
        assert_eq!(
            (cpu.n_flag, cpu.not_z_flag, cpu.v_flag, cpu.c_flag),
            old_flags
        );
    }
}
