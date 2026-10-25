//! Decoded simple operation cache.
//!
//! Caches compact decoded operations for simple one-word instructions and
//! FastMem-backed memory operations. The same operations feed the portable
//! trace executor and, with the `jit` feature on native targets, Cranelift.
//! One-word short branches are included because their fetch timing is fully
//! local to the current instruction.
//! Instruction fetch still occurs at the normal instruction boundary, so bus/address-error timing
//! stays aligned with the interpreter.

use super::cpu::CpuCore;
use super::execute::{RUN_MODE_BERR_AERR_RESET, RUN_MODE_NORMAL};
use super::memory::AddressBus;
use super::trace_jit;
use super::trace_jit::{JitAddrOp, JitBinaryOp, JitBitOp, JitDirectReg, JitTraceOp, JitUnaryOp};
use super::types::{CpuType, Size};

/// Number of entries in the opcode-indexed decode table: one per possible
/// opcode word. Decode depends only on `(opcode, cpu_type)`, so the table
/// can never go stale from self-modifying code — the fetched opcode itself
/// is the index.
pub(crate) const DECODE_TABLE_SIZE: usize = 1 << 16;

/// Cached decode verdict for one opcode word.
///
/// `Complex` is a cached negative: memory-heavy code revisits the same
/// opcodes constantly, so remembering a rejection is as valuable as
/// remembering a hit.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CachedOp {
    /// Not decoded yet (table sentinel).
    Unknown,
    /// Register-only one-word op — runs on every fast path, exact cycles.
    Simple(DecodedSimpleOp),
    /// Memory-operand op — runs only in the instruction-budgeted batch
    /// loop when a fastmem window is active (no cycle accounting).
    Mem(super::mem_ops::DecodedMemOp),
    /// Neither: full dispatch.
    Complex,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DecodedSimpleOp {
    Nop,
    MoveReg {
        src: DirectReg,
        dst: DirectReg,
        size: Size,
    },
    Moveq {
        reg: u8,
        data: u32,
    },
    UnaryDataReg {
        op: UnaryOp,
        reg: u8,
        size: Size,
    },
    Swap {
        reg: u8,
    },
    Ext {
        reg: u8,
        size: Size,
    },
    Extb {
        reg: u8,
    },
    AddqSubqReg {
        reg: u8,
        data: u32,
        size: Size,
        is_sub: bool,
    },
    AddqSubqAddr {
        reg: u8,
        data: u32,
        is_sub: bool,
    },
    BinaryDataReg {
        op: BinaryOp,
        src: DirectReg,
        dst: u8,
        size: Size,
        cycles: i32,
    },
    MulWordDataReg {
        src: u8,
        dst: u8,
        signed: bool,
        m68000_timing: bool,
    },
    AddrDataReg {
        op: AddrOp,
        src: DirectReg,
        dst: u8,
        size: Size,
    },
    AddSubxReg {
        src: u8,
        dst: u8,
        size: Size,
        is_sub: bool,
    },
    BitReg {
        op: BitOp,
        bit_reg: u8,
        dst: u8,
    },
    BcdReg {
        src: u8,
        dst: u8,
        is_sub: bool,
    },
    Exg {
        opcode: u16,
    },
    SccDataReg {
        condition: u8,
        reg: u8,
    },
    ShiftReg {
        reg: u8,
        size: Size,
        count_or_reg: u8,
        count_is_register: bool,
        direction: u8,
        op: u8,
    },
    BranchShort {
        condition: u8,
        displacement: i8,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DirectReg {
    Data(u8),
    Addr(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Clr,
    Neg,
    Negx,
    Not,
    Tst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Sub,
    And,
    Or,
    Eor,
    Cmp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddrOp {
    Adda,
    Suba,
    Cmpa,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BitOp {
    Test,
    Change,
    Clear,
    Set,
}

pub(crate) enum CachedRunResult {
    Ran,
    Miss(u16),
}

/// Result of the instruction-budgeted fast loop used by `run_batch`.
pub(crate) enum BatchInnerExit {
    /// The embedder requested synchronization before the next instruction.
    Boundary,
    /// The remaining instruction budget hit zero.
    Budget,
    /// Execution reached a PC in the caller's watch list.
    Watched(u32),
    /// A bus/address error was taken during fetch or trace execution.
    Fault,
    /// The instruction at the current PC is not a cached simple op;
    /// the opcode has been fetched (`ppc`/`ir` are set) and full
    /// dispatch should handle it.
    Miss(u16),
}

impl DecodedSimpleOp {
    #[inline]
    pub(crate) fn decode(cpu_type: CpuType, opcode: u16) -> Option<Self> {
        let group = (opcode >> 12) & 0xF;

        if (1..=3).contains(&group) {
            return decode_move_reg(
                opcode,
                match group {
                    1 => Size::Byte,
                    2 => Size::Long,
                    3 => Size::Word,
                    _ => unreachable!(),
                },
            );
        }

        if group == 0x7 {
            return Some(Self::Moveq {
                reg: ((opcode >> 9) & 7) as u8,
                data: (opcode & 0xFF) as i8 as i32 as u32,
            });
        }

        if opcode == 0x4E71 {
            return Some(Self::Nop);
        }

        if group == 0x4
            && let Some(op) = decode_group_4_reg(cpu_type, opcode)
        {
            return Some(op);
        }

        if group == 0x0 && (opcode & 0x0100) != 0 && ((opcode >> 3) & 7) == 0 {
            let op = match (opcode >> 6) & 3 {
                0 => BitOp::Test,
                1 => BitOp::Change,
                2 => BitOp::Clear,
                3 => BitOp::Set,
                _ => unreachable!(),
            };
            return Some(Self::BitReg {
                op,
                bit_reg: ((opcode >> 9) & 7) as u8,
                dst: (opcode & 7) as u8,
            });
        }

        if (opcode & 0xFFF8) == 0x4840 {
            return Some(Self::Swap {
                reg: (opcode & 7) as u8,
            });
        }

        if (opcode & 0xFFF8) == 0x4880 {
            return Some(Self::Ext {
                reg: (opcode & 7) as u8,
                size: Size::Word,
            });
        }

        if (opcode & 0xFFF8) == 0x48C0 {
            return Some(Self::Ext {
                reg: (opcode & 7) as u8,
                size: Size::Long,
            });
        }

        if (opcode & 0xFFF8) == 0x49C0 && !is_pre_68020(cpu_type) {
            return Some(Self::Extb {
                reg: (opcode & 7) as u8,
            });
        }

        if group == 0x5 && ((opcode >> 6) & 3) != 3 {
            let ea_mode = (opcode >> 3) & 7;
            if ea_mode <= 1 {
                let data = ((opcode >> 9) & 7) as u32;
                let data = if data == 0 { 8 } else { data };
                let is_sub = (opcode & 0x100) != 0;
                let reg = (opcode & 7) as u8;
                if ea_mode == 1 {
                    return Some(Self::AddqSubqAddr { reg, data, is_sub });
                }
                return Some(Self::AddqSubqReg {
                    reg,
                    data,
                    size: decode_size_00((opcode >> 6) & 3),
                    is_sub,
                });
            }
        }

        if group == 0x5 && ((opcode >> 6) & 3) == 3 && ((opcode >> 3) & 7) == 0 {
            return Some(Self::SccDataReg {
                condition: ((opcode >> 8) & 0xF) as u8,
                reg: (opcode & 7) as u8,
            });
        }

        if matches!(group, 0x8 | 0x9 | 0xB | 0xC | 0xD)
            && let Some(op) = decode_group_alu_reg(cpu_type, opcode)
        {
            return Some(op);
        }

        if group == 0xE && (opcode & 0x00C0) != 0x00C0 {
            return Some(Self::ShiftReg {
                reg: (opcode & 7) as u8,
                size: decode_size_00((opcode >> 6) & 3),
                count_or_reg: ((opcode >> 9) & 7) as u8,
                count_is_register: (opcode & 0x20) != 0,
                direction: ((opcode >> 8) & 1) as u8,
                op: ((opcode >> 3) & 3) as u8,
            });
        }

        if group == 0x6 {
            let condition = ((opcode >> 8) & 0xF) as u8;
            let displacement = (opcode & 0xFF) as u8;
            if condition != 1 && displacement != 0 && displacement != 0xFF {
                return Some(Self::BranchShort {
                    condition,
                    displacement: displacement as i8,
                });
            }
        }

        None
    }

    #[inline]
    pub(crate) fn to_jit_trace_op(self) -> Option<JitTraceOp> {
        match self {
            Self::Nop => Some(JitTraceOp::Nop),
            Self::MoveReg { src, dst, size } => Some(JitTraceOp::MoveReg {
                src: jit_direct_reg(src),
                dst: jit_direct_reg(dst),
                size,
            }),
            Self::Moveq { reg, data } => Some(JitTraceOp::Moveq { reg, data }),
            Self::UnaryDataReg { op, reg, size } => Some(JitTraceOp::UnaryDataReg {
                op: jit_unary_op(op),
                reg,
                size,
            }),
            Self::Swap { reg } => Some(JitTraceOp::Swap { reg }),
            Self::Ext { reg, size } => Some(JitTraceOp::Ext { reg, size }),
            Self::Extb { reg } => Some(JitTraceOp::Extb { reg }),
            Self::AddqSubqReg {
                reg,
                data,
                size,
                is_sub,
            } => Some(JitTraceOp::AddqSubqReg {
                reg,
                data,
                size,
                is_sub,
            }),
            Self::AddqSubqAddr { reg, data, is_sub } => {
                Some(JitTraceOp::AddqSubqAddr { reg, data, is_sub })
            }
            Self::BinaryDataReg {
                op,
                src,
                dst,
                size,
                cycles,
            } => Some(JitTraceOp::BinaryDataReg {
                op: jit_binary_op(op),
                src: jit_direct_reg(src),
                dst,
                size,
                cycles,
            }),
            Self::MulWordDataReg {
                src,
                dst,
                signed,
                m68000_timing,
            } => Some(JitTraceOp::MulWordDataReg {
                src,
                dst,
                signed,
                m68000_timing,
            }),
            Self::AddrDataReg { op, src, dst, size } => Some(JitTraceOp::AddrDataReg {
                op: jit_addr_op(op),
                src: jit_direct_reg(src),
                dst,
                size,
            }),
            Self::AddSubxReg {
                src,
                dst,
                size,
                is_sub,
            } => Some(JitTraceOp::AddSubxReg {
                src,
                dst,
                size,
                is_sub,
            }),
            Self::BitReg { op, bit_reg, dst } => Some(JitTraceOp::BitReg {
                op: jit_bit_op(op),
                bit_reg,
                dst,
            }),
            Self::Exg { opcode } => Some(JitTraceOp::Exg { opcode }),
            Self::SccDataReg { condition, reg } => Some(JitTraceOp::SccDataReg { condition, reg }),
            Self::ShiftReg {
                reg,
                size,
                count_or_reg,
                count_is_register,
                direction,
                op,
            } => {
                #[cfg(any(not(feature = "jit"), target_family = "wasm"))]
                {
                    Some(JitTraceOp::ShiftReg {
                        reg,
                        size,
                        count_or_reg,
                        count_is_register,
                        direction,
                        op,
                    })
                }
                #[cfg(all(feature = "jit", not(target_family = "wasm")))]
                {
                    // Native traces lower the right arithmetic/logical and
                    // left logical forms, with either an immediate or a
                    // register count. ASL and the rotates stay on the decoded
                    // interpreter until their overflow and extend handling is
                    // lowered and measured independently.
                    if matches!((op, direction), (0, 0) | (1, 0) | (1, 1)) {
                        Some(JitTraceOp::ShiftReg {
                            reg,
                            size,
                            count_or_reg,
                            count_is_register,
                            direction,
                            op,
                        })
                    } else {
                        None
                    }
                }
            }
            Self::BranchShort {
                condition,
                displacement,
            } => Some(JitTraceOp::Branch {
                condition,
                displacement: displacement as i32,
                length: 2,
                expected_taken: None,
            }),
            _ => None,
        }
    }

    #[inline(always)]
    pub(crate) fn execute<B: AddressBus>(self, cpu: &mut CpuCore, bus: &mut B) -> i32 {
        match self {
            Self::Nop => 4,
            Self::MoveReg { src, dst, size } => {
                let value = read_direct_reg(cpu, src, size);
                match dst {
                    DirectReg::Data(reg) => {
                        let reg = reg as usize;
                        write_data_reg(cpu, reg, size, value);
                        cpu.set_logic_flags(value, size);
                    }
                    DirectReg::Addr(reg) => {
                        let reg = reg as usize;
                        let value = if size == Size::Word {
                            value as i16 as i32 as u32
                        } else {
                            value
                        };
                        cpu.dar[8 + reg] = value;
                    }
                }
                4
            }
            Self::Moveq { reg, data } => {
                let reg = reg as usize;
                cpu.dar[reg] = data;
                cpu.n_flag = if (data as i32) < 0 { 0x80 } else { 0 };
                cpu.not_z_flag = data;
                cpu.v_flag = 0;
                cpu.c_flag = 0;
                4
            }
            Self::UnaryDataReg { op, reg, size } => {
                let reg = reg as usize;
                let mask = size.mask();
                let src = cpu.dar[reg] & mask;
                match op {
                    UnaryOp::Clr => {
                        write_data_reg(cpu, reg, size, 0);
                        cpu.n_flag = 0;
                        cpu.not_z_flag = 0;
                        cpu.v_flag = 0;
                        cpu.c_flag = 0;
                    }
                    UnaryOp::Neg => {
                        let result = 0u32.wrapping_sub(src);
                        write_data_reg(cpu, reg, size, result);
                        cpu.set_sub_flags(src, 0, result, size);
                    }
                    UnaryOp::Negx => {
                        let result = cpu.exec_subx(size, src, 0);
                        write_data_reg(cpu, reg, size, result);
                    }
                    UnaryOp::Not => {
                        let result = !src & mask;
                        write_data_reg(cpu, reg, size, result);
                        cpu.set_logic_flags(result, size);
                    }
                    UnaryOp::Tst => {
                        cpu.set_logic_flags(src, size);
                    }
                }
                if cpu.is_pre_68020 && size == Size::Long && op != UnaryOp::Tst {
                    6
                } else {
                    4
                }
            }
            Self::Swap { reg } => cpu.exec_swap(bus, reg as usize),
            Self::Ext { reg, size } => cpu.exec_ext(size, reg as usize),
            Self::Extb { reg } => cpu.exec_extb(reg as usize),
            Self::AddqSubqAddr { reg, data, is_sub } => {
                let reg = 8 + reg as usize;
                if is_sub {
                    cpu.dar[reg] = cpu.dar[reg].wrapping_sub(data);
                } else {
                    cpu.dar[reg] = cpu.dar[reg].wrapping_add(data);
                }
                if cpu.is_pre_68020 { 8 } else { 4 }
            }
            Self::AddqSubqReg {
                reg,
                data,
                size,
                is_sub,
            } => {
                let reg = reg as usize;
                let mask = size.mask();
                let dst = cpu.dar[reg] & mask;
                let result = if is_sub {
                    let result = dst.wrapping_sub(data);
                    cpu.set_sub_flags(data, dst, result, size);
                    result & mask
                } else {
                    let result = dst.wrapping_add(data);
                    cpu.set_add_flags(data, dst, result, size);
                    result & mask
                };
                cpu.dar[reg] = (cpu.dar[reg] & !mask) | result;
                if cpu.is_pre_68020 && size == Size::Long {
                    8
                } else {
                    4
                }
            }
            Self::BinaryDataReg {
                op,
                src,
                dst,
                size,
                cycles,
            } => {
                let dst = dst as usize;
                let mask = size.mask();
                let src = read_direct_reg(cpu, src, size);
                let dst_value = cpu.dar[dst] & mask;
                match op {
                    BinaryOp::Add => {
                        let result = dst_value.wrapping_add(src);
                        cpu.set_add_flags(src, dst_value, result, size);
                        write_data_reg(cpu, dst, size, result);
                    }
                    BinaryOp::Sub => {
                        let result = dst_value.wrapping_sub(src);
                        cpu.set_sub_flags(src, dst_value, result, size);
                        write_data_reg(cpu, dst, size, result);
                    }
                    BinaryOp::And => {
                        let result = (src & dst_value) & mask;
                        cpu.set_logic_flags(result, size);
                        write_data_reg(cpu, dst, size, result);
                    }
                    BinaryOp::Or => {
                        let result = (src | dst_value) & mask;
                        cpu.set_logic_flags(result, size);
                        write_data_reg(cpu, dst, size, result);
                    }
                    BinaryOp::Eor => {
                        let result = (src ^ dst_value) & mask;
                        cpu.set_logic_flags(result, size);
                        write_data_reg(cpu, dst, size, result);
                    }
                    BinaryOp::Cmp => {
                        let result = dst_value.wrapping_sub(src);
                        cpu.set_cmp_flags(src, dst_value, result, size);
                    }
                }
                cycles
            }
            Self::MulWordDataReg {
                src,
                dst,
                signed,
                m68000_timing,
            } => {
                let src_word = cpu.dar[src as usize] as u16;
                let dst_word = cpu.dar[dst as usize] as u16;
                let result = if signed {
                    (i32::from(src_word as i16) * i32::from(dst_word as i16)) as u32
                } else {
                    u32::from(src_word) * u32::from(dst_word)
                };
                cpu.dar[dst as usize] = result;
                cpu.set_logic_flags(result, Size::Long);

                if m68000_timing {
                    let variable = if signed {
                        // The 68000 signed multiplier counts transitions in
                        // the 17-bit sequence formed by the source and a
                        // trailing zero. Later CPUs use fixed timing.
                        let shifted = u32::from(src_word) << 1;
                        ((shifted ^ (shifted >> 1)) & 0xFFFF).count_ones()
                    } else {
                        src_word.count_ones()
                    };
                    38 + 2 * variable as i32
                } else {
                    42
                }
            }
            Self::AddrDataReg { op, src, dst, size } => {
                let dst = dst as usize;
                let mut src = read_direct_reg(cpu, src, size);
                if size == Size::Word {
                    src = src as i16 as i32 as u32;
                }
                let dst_value = cpu.dar[8 + dst];
                match op {
                    AddrOp::Adda => {
                        cpu.dar[8 + dst] = dst_value.wrapping_add(src);
                        8
                    }
                    AddrOp::Suba => {
                        cpu.dar[8 + dst] = dst_value.wrapping_sub(src);
                        8
                    }
                    AddrOp::Cmpa => {
                        let result = dst_value.wrapping_sub(src);
                        cpu.set_cmp_flags(src, dst_value, result, Size::Long);
                        6
                    }
                }
            }
            Self::AddSubxReg {
                src,
                dst,
                size,
                is_sub,
            } => {
                let src = src as usize;
                let dst = dst as usize;
                let mask = size.mask();
                let src = cpu.dar[src] & mask;
                let dst_value = cpu.dar[dst] & mask;
                let result = if is_sub {
                    cpu.exec_subx(size, src, dst_value)
                } else {
                    cpu.exec_addx(size, src, dst_value)
                };
                write_data_reg(cpu, dst, size, result);
                if cpu.is_pre_68020 && size == Size::Long {
                    8
                } else {
                    4
                }
            }
            Self::BitReg { op, bit_reg, dst } => {
                let bit = cpu.dar[bit_reg as usize] & 31;
                let mask = 1u32 << bit;
                let dst = dst as usize;
                let value = cpu.dar[dst];
                cpu.not_z_flag = if value & mask != 0 { 1 } else { 0 };
                let hi_bit_extra = if cpu.is_pre_68020 && bit >= 16 { 2 } else { 0 };
                match op {
                    BitOp::Test => 6,
                    BitOp::Change => {
                        cpu.dar[dst] = value ^ mask;
                        if cpu.is_pre_68020 {
                            6 + hi_bit_extra
                        } else {
                            8
                        }
                    }
                    BitOp::Clear => {
                        cpu.dar[dst] = value & !mask;
                        if cpu.is_pre_68020 {
                            8 + hi_bit_extra
                        } else {
                            10
                        }
                    }
                    BitOp::Set => {
                        cpu.dar[dst] = value | mask;
                        if cpu.is_pre_68020 {
                            6 + hi_bit_extra
                        } else {
                            8
                        }
                    }
                }
            }
            Self::BcdReg { src, dst, is_sub } => {
                if is_sub {
                    cpu.exec_sbcd_rr(bus, src as usize, dst as usize)
                } else {
                    cpu.exec_abcd_rr(bus, src as usize, dst as usize)
                }
            }
            Self::Exg { opcode } => cpu.exec_exg(bus, opcode),
            Self::SccDataReg { condition, reg } => {
                let reg = reg as usize;
                let value = if cpu.test_condition(condition) {
                    0xFF
                } else {
                    0
                };
                write_data_reg(cpu, reg, Size::Byte, value);
                if cpu.is_pre_68020 && value != 0 { 6 } else { 4 }
            }
            Self::ShiftReg {
                reg,
                size,
                count_or_reg,
                count_is_register,
                direction,
                op,
            } => {
                let reg = reg as usize;
                let shift = if count_is_register {
                    cpu.dar[count_or_reg as usize] & 63
                } else {
                    let c = count_or_reg as u32;
                    if c == 0 { 8 } else { c }
                };
                let value = cpu.dar[reg] & size.mask();
                let (result, cycles) = match (op, direction) {
                    (0, 0) => cpu.exec_asr(size, shift, value),
                    (0, 1) => cpu.exec_asl(size, shift, value),
                    (1, 0) => cpu.exec_lsr(size, shift, value),
                    (1, 1) => cpu.exec_lsl(size, shift, value),
                    (2, 0) => cpu.exec_roxr(size, shift, value),
                    (2, 1) => cpu.exec_roxl(size, shift, value),
                    (3, 0) => cpu.exec_ror(size, shift, value),
                    (3, 1) => cpu.exec_rol(size, shift, value),
                    _ => unreachable!(),
                };
                let mask = size.mask();
                cpu.dar[reg] = (cpu.dar[reg] & !mask) | result;
                cycles
            }
            Self::BranchShort {
                condition,
                displacement,
            } => {
                if condition == 0 || cpu.test_condition(condition) {
                    cpu.change_of_flow = true;
                    cpu.pc = (cpu.pc as i32).wrapping_add(displacement as i32) as u32;
                    10
                } else {
                    8
                }
            }
        }
    }
}

#[inline]
fn decode_move_reg(opcode: u16, size: Size) -> Option<DecodedSimpleOp> {
    let src = direct_reg((opcode >> 3) & 7, opcode & 7)?;
    let dst_reg = ((opcode >> 9) & 7) as u8;
    let dst_mode = (opcode >> 6) & 7;

    let dst = match dst_mode {
        0 => DirectReg::Data(dst_reg),
        1 if size != Size::Byte => DirectReg::Addr(dst_reg),
        _ => return None,
    };

    Some(DecodedSimpleOp::MoveReg { src, dst, size })
}

#[inline]
fn decode_group_4_reg(_cpu_type: CpuType, opcode: u16) -> Option<DecodedSimpleOp> {
    let ea_mode = (opcode >> 3) & 7;
    if ea_mode != 0 {
        return None;
    }

    let size_bits = (opcode >> 6) & 3;
    if size_bits == 3 {
        return None;
    }

    let op = match (opcode >> 8) & 0xF {
        0x0 => UnaryOp::Negx,
        0x2 => UnaryOp::Clr,
        0x4 => UnaryOp::Neg,
        0x6 => UnaryOp::Not,
        0xA => UnaryOp::Tst,
        _ => return None,
    };

    Some(DecodedSimpleOp::UnaryDataReg {
        op,
        reg: (opcode & 7) as u8,
        size: decode_size_00(size_bits),
    })
}

#[inline]
fn decode_group_alu_reg(cpu_type: CpuType, opcode: u16) -> Option<DecodedSimpleOp> {
    // Register-to-register ALU base times (68000/68010): byte/word 4;
    // long 8 (register/immediate source footnote), except CMP long at 6.
    let pre020 = is_pre_68020(cpu_type);
    let alu_cycles = |size_bits: u16, cmp: bool| -> i32 {
        let long = size_bits == 2;
        if pre020 {
            match (long, cmp) {
                (false, _) => 4,
                (true, false) => 8,
                (true, true) => 6,
            }
        } else {
            4
        }
    };
    let group = (opcode >> 12) & 0xF;
    let reg = ((opcode >> 9) & 7) as u8;
    let ea_mode = (opcode >> 3) & 7;
    let ea_reg = (opcode & 7) as u8;
    let op_mode = (opcode >> 6) & 7;
    let src = direct_reg(ea_mode, opcode & 7);

    match group {
        0x8 => {
            if op_mode <= 2 {
                Some(DecodedSimpleOp::BinaryDataReg {
                    op: BinaryOp::Or,
                    src: src?,
                    dst: reg,
                    size: decode_size_012(op_mode),
                    cycles: alu_cycles(op_mode, false),
                })
            } else if op_mode == 4 && ea_mode == 0 {
                Some(DecodedSimpleOp::BcdReg {
                    src: ea_reg,
                    dst: reg,
                    is_sub: true,
                })
            } else {
                None
            }
        }
        0x9 => match op_mode {
            0..=2 => Some(DecodedSimpleOp::BinaryDataReg {
                op: BinaryOp::Sub,
                src: src?,
                dst: reg,
                size: decode_size_012(op_mode),
                cycles: alu_cycles(op_mode, false),
            }),
            3 | 7 => Some(DecodedSimpleOp::AddrDataReg {
                op: AddrOp::Suba,
                src: src?,
                dst: reg,
                size: if op_mode == 3 { Size::Word } else { Size::Long },
            }),
            4..=6 if ea_mode == 0 => Some(DecodedSimpleOp::AddSubxReg {
                src: ea_reg,
                dst: reg,
                size: decode_size_012(op_mode - 4),
                is_sub: true,
            }),
            _ => None,
        },
        0xB => match op_mode {
            0..=2 => Some(DecodedSimpleOp::BinaryDataReg {
                op: BinaryOp::Cmp,
                src: src?,
                dst: reg,
                size: decode_size_012(op_mode),
                cycles: alu_cycles(op_mode, true),
            }),
            3 | 7 => Some(DecodedSimpleOp::AddrDataReg {
                op: AddrOp::Cmpa,
                src: src?,
                dst: reg,
                size: if op_mode == 3 { Size::Word } else { Size::Long },
            }),
            4..=6 if ea_mode == 0 => Some(DecodedSimpleOp::BinaryDataReg {
                op: BinaryOp::Eor,
                src: DirectReg::Data(reg),
                dst: ea_reg,
                size: decode_size_012(op_mode - 4),
                cycles: if pre020 {
                    alu_cycles(op_mode - 4, false)
                } else {
                    8
                },
            }),
            _ => None,
        },
        0xC => {
            if matches!(op_mode, 3 | 7) && ea_mode == 0 {
                return Some(DecodedSimpleOp::MulWordDataReg {
                    src: ea_reg,
                    dst: reg,
                    signed: op_mode == 7,
                    m68000_timing: cpu_type == CpuType::M68000,
                });
            }

            if op_mode <= 2 {
                return Some(DecodedSimpleOp::BinaryDataReg {
                    op: BinaryOp::And,
                    src: src?,
                    dst: reg,
                    size: decode_size_012(op_mode),
                    cycles: alu_cycles(op_mode, false),
                });
            }

            if op_mode == 4 && ea_mode == 0 {
                return Some(DecodedSimpleOp::BcdReg {
                    src: ea_reg,
                    dst: reg,
                    is_sub: false,
                });
            }

            let mode_field = (opcode >> 3) & 0x1F;
            if matches!(mode_field, 0x08 | 0x09 | 0x11) {
                Some(DecodedSimpleOp::Exg { opcode })
            } else {
                None
            }
        }
        0xD => match op_mode {
            0..=2 => Some(DecodedSimpleOp::BinaryDataReg {
                op: BinaryOp::Add,
                src: src?,
                dst: reg,
                size: decode_size_012(op_mode),
                cycles: alu_cycles(op_mode, false),
            }),
            3 | 7 => Some(DecodedSimpleOp::AddrDataReg {
                op: AddrOp::Adda,
                src: src?,
                dst: reg,
                size: if op_mode == 3 { Size::Word } else { Size::Long },
            }),
            4..=6 if ea_mode == 0 => Some(DecodedSimpleOp::AddSubxReg {
                src: ea_reg,
                dst: reg,
                size: decode_size_012(op_mode - 4),
                is_sub: false,
            }),
            _ => None,
        },
        _ => None,
    }
}

impl CpuCore {
    /// Drop the decode table. Only needed when `cpu_type` changes (decode
    /// results depend on it); self-modifying code cannot stale the table.
    #[inline]
    pub(crate) fn clear_decoded_op_cache(&mut self) {
        self.decode_table = None;
    }

    #[inline]
    pub(crate) fn can_run_decoded_simple_ops(&self) -> bool {
        self.run_mode == RUN_MODE_NORMAL
            && self.stopped == 0
            && self.int_level == 0
            && self.t1_flag == 0
            && self.t0_flag == 0
    }

    /// Instruction-budgeted decoded/JIT loop for
    /// [`CpuCore::run_batch`](crate::CpuCore::run_batch).
    ///
    /// Its budget counts retired instructions, and `*retired` is
    /// incremented in place so the caller keeps an exact count even
    /// when JIT traces retire many instructions per call.
    ///
    /// Additional rules:
    /// - `budget` counts retired instructions, and `*retired` is
    ///   incremented in place so the caller keeps an exact count even
    ///   when JIT traces retire many instructions per call.
    /// - Traces are only attempted while at least `TRACE_MIN_OPS`
    ///   instructions remain; the trace itself checks its exact length
    ///   before entry, so it cannot overshoot the budget.
    /// - After every retired instruction (or trace), the new PC is
    ///   checked against `watch_pcs`.
    pub(crate) fn run_decoded_simple_batch<B: AddressBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        watch_pcs: &[u32],
        retired: &mut u32,
        probe_on_entry: bool,
    ) -> BatchInnerExit {
        // Only a fully dispatched instruction, outside this loop, switches
        // the window between physical and translated, so the loop is
        // specialized for the mode it starts in; the physical one compiles
        // as it did before translation existed.
        if self.fm_mmu.enabled() {
            return self.run_translated_simple_batch(
                bus,
                budget,
                watch_pcs,
                retired,
                probe_on_entry,
            );
        }
        self.run_decoded_simple_batch_in::<B, false>(
            bus,
            budget,
            watch_pcs,
            retired,
            probe_on_entry,
        )
    }

    /// The decoded loop through a translated window. Cold only as a branch
    /// weight for the caller: with no profile LLVM split the batch path
    /// evenly between the two loops, which halved every block frequency in
    /// the physical one, pushed its rarer operation arms under the cold
    /// call-site inlining threshold and laid it out worse. Block frequencies
    /// inside this loop are relative to its own entry, so it compiles the
    /// same either way.
    #[cold]
    #[inline(never)]
    fn run_translated_simple_batch<B: AddressBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        watch_pcs: &[u32],
        retired: &mut u32,
        probe_on_entry: bool,
    ) -> BatchInnerExit {
        // Whatever ran since the last decoded run may have changed the ATC.
        self.forget_code_page();
        self.run_decoded_simple_batch_in::<B, true>(bus, budget, watch_pcs, retired, probe_on_entry)
    }

    #[inline]
    fn run_decoded_simple_batch_in<B: AddressBus, const TRANSLATED: bool>(
        &mut self,
        bus: &mut B,
        budget: u32,
        watch_pcs: &[u32],
        retired: &mut u32,
        probe_on_entry: bool,
    ) -> BatchInnerExit {
        let cpu_type = self.cpu_type;
        let watch = !watch_pcs.is_empty();
        let mut remaining = budget;
        // See `execute_decoded_simple_run`: probe the trace cache only on
        // entry and after backward branches, never per instruction.
        let mut probe = probe_on_entry && trace_jit::has_trace_candidates();

        while remaining > 0 {
            if bus.batch_boundary(*retired, self.pc, self.get_sr()) {
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchInnerExit::Boundary;
            }
            if probe && remaining >= trace_jit::TRACE_MIN_OPS as u32 {
                // run_batch is instruction-budgeted, not cycle-budgeted.
                // A long-running self-loop can consume the synthetic cycle
                // headroom in one trace call; replenish it before every
                // probe so subsequent iterations do not silently fall back
                // to the interpreter. Cycle-budgeted callers use the other
                // decoded-op loop and retain their real remaining budget.
                self.cycles_remaining = i32::MAX / 2;
                probe = false;
                // A self-loop only needs to stop after one iteration when
                // returning to its own head would hit a watched PC. Merely
                // having an unrelated watch (for example, a clean-exit
                // sentinel at PC 0) must not serialize every JIT iteration.
                let single_iter = watch && watch_pcs.contains(&self.pc);
                if let Some((result, instructions)) = trace_jit::try_execute_trace::<B, TRANSLATED>(
                    self,
                    bus,
                    cpu_type,
                    remaining,
                    single_iter,
                    watch_pcs,
                ) {
                    match result {
                        CachedRunResult::Ran => {
                            remaining -= instructions;
                            *retired += instructions;
                            if watch && watch_pcs.contains(&self.pc) {
                                return BatchInnerExit::Watched(self.pc);
                            }
                            probe = true;
                            continue;
                        }
                        CachedRunResult::Miss(opcode) => {
                            // A chained continuation can retire instructions
                            // before its successor misses validation; those
                            // are real completed instructions and must be
                            // accounted before the missed opcode dispatches.
                            // (`remaining` needs no update: this return path
                            // leaves the budget loop entirely.)
                            *retired += instructions;
                            return BatchInnerExit::Miss(opcode);
                        }
                    }
                }
            }

            self.ppc = self.pc;
            let opcode = if self.fm_len != 0 && (self.pc & 1) == 0 {
                // Fetch through the fastmem window: one bounds check
                // instead of a bus call. Odd/out-of-window PCs, and PCs the
                // PMMU cannot translate without a table search, take the
                // normal fetch path (and its fault handling).
                let addr = self.address(self.pc);
                let off = if TRANSLATED {
                    self.code_window_off(addr)
                } else {
                    self.physical_window_off(addr, 2, false)
                };
                if let Some(off) = off {
                    let opcode = unsafe {
                        let p = self.window_host_ptr(off);
                        u16::from_be_bytes([*p, *p.add(1)])
                    };
                    self.pc = self.pc.wrapping_add(2);
                    opcode
                } else {
                    let opcode = self.read_opcode_16(bus);
                    if TRANSLATED {
                        // A table search may have replaced the remembered
                        // page.
                        self.forget_code_page();
                    }
                    if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                        return BatchInnerExit::Fault;
                    }
                    opcode
                }
            } else {
                let opcode = self.read_opcode_16(bus);
                if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                    return BatchInnerExit::Fault;
                }
                opcode
            };
            self.ir = opcode as u32;

            match self.cached_decode(opcode, cpu_type) {
                CachedOp::Unknown => unreachable!("cached_decode never returns Unknown"),
                CachedOp::Simple(op) => {
                    let branch_pc = if matches!(op, DecodedSimpleOp::BranchShort { .. }) {
                        Some(self.ppc)
                    } else {
                        None
                    };
                    let _cycles = op.execute(self, bus);
                    trace_jit::record_executed(self, bus, self.ppc, self.pc);
                    if let Some(branch_pc) = branch_pc
                        && self.pc <= branch_pc
                    {
                        probe = trace_jit::note_backward_branch(self, cpu_type);
                    }
                }
                CachedOp::Mem(op) => {
                    if !super::mem_ops::execute_window_mem_op::<TRANSLATED>(self, op) {
                        // Tracked windows intentionally use full dispatch for
                        // this instruction, whose successful completion is
                        // recorded by run_batch_inner. This is not a host yield.
                        if self.fm_write_hook == 0 {
                            trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                        }
                        return BatchInnerExit::Miss(opcode);
                    }
                    #[cfg(feature = "trace-profile")]
                    super::trace_profile::note_decoded_mem(self.ppc, opcode);
                    trace_jit::record_executed(self, bus, self.ppc, self.pc);
                    if self.pc <= self.ppc {
                        probe = trace_jit::note_backward_branch(self, cpu_type);
                    }
                }
                CachedOp::Complex => {
                    return BatchInnerExit::Miss(opcode);
                }
            }
            remaining -= 1;
            *retired += 1;
            if watch && watch_pcs.contains(&self.pc) {
                trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
                return BatchInnerExit::Watched(self.pc);
            }
        }

        trace_jit::stop_recording(self, trace_jit::RecordingStop::HostBoundary);
        BatchInnerExit::Budget
    }

    /// Table-backed decode used by every fast path: a direct load indexed
    /// by the opcode word, filled on first sight of each opcode value.
    ///
    /// `cpu_type` must be the core's current type (the table is dropped on
    /// type changes).
    #[inline]
    pub(crate) fn cached_decode(&mut self, opcode: u16, cpu_type: CpuType) -> CachedOp {
        debug_assert_eq!(cpu_type, self.cpu_type);
        let table = match &mut self.decode_table {
            Some(table) => table,
            None => {
                // Lazily allocated so cores that only ever run the plain
                // interpreter paths do not pay for it up front.
                let table: Box<[CachedOp]> = vec![CachedOp::Unknown; DECODE_TABLE_SIZE].into();
                let table: Box<[CachedOp; DECODE_TABLE_SIZE]> = table
                    .try_into()
                    .expect("table has DECODE_TABLE_SIZE entries");
                self.decode_table.insert(table)
            }
        };
        // `u16` index into a 1<<16 array: no bounds check.
        let entry = table[opcode as usize];
        if !matches!(entry, CachedOp::Unknown) {
            return entry;
        }

        let op = match DecodedSimpleOp::decode(cpu_type, opcode) {
            Some(op) => CachedOp::Simple(op),
            None => match super::mem_ops::DecodedMemOp::decode(cpu_type, opcode) {
                Some(op) => CachedOp::Mem(op),
                None => CachedOp::Complex,
            },
        };
        table[opcode as usize] = op;
        op
    }
}

#[inline]
fn decode_size_00(bits: u16) -> Size {
    match bits {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => Size::Byte,
    }
}

#[inline]
fn decode_size_012(bits: u16) -> Size {
    match bits {
        0 => Size::Byte,
        1 => Size::Word,
        2 => Size::Long,
        _ => Size::Byte,
    }
}

#[inline]
fn direct_reg(mode: u16, reg: u16) -> Option<DirectReg> {
    match mode {
        0 => Some(DirectReg::Data(reg as u8)),
        1 => Some(DirectReg::Addr(reg as u8)),
        _ => None,
    }
}

#[inline]
fn jit_direct_reg(reg: DirectReg) -> JitDirectReg {
    match reg {
        DirectReg::Data(reg) => JitDirectReg::Data(reg),
        DirectReg::Addr(reg) => JitDirectReg::Addr(reg),
    }
}

#[inline]
fn jit_unary_op(op: UnaryOp) -> JitUnaryOp {
    match op {
        UnaryOp::Clr => JitUnaryOp::Clr,
        UnaryOp::Neg => JitUnaryOp::Neg,
        UnaryOp::Negx => JitUnaryOp::Negx,
        UnaryOp::Not => JitUnaryOp::Not,
        UnaryOp::Tst => JitUnaryOp::Tst,
    }
}

#[inline]
fn jit_binary_op(op: BinaryOp) -> JitBinaryOp {
    match op {
        BinaryOp::Add => JitBinaryOp::Add,
        BinaryOp::Sub => JitBinaryOp::Sub,
        BinaryOp::And => JitBinaryOp::And,
        BinaryOp::Or => JitBinaryOp::Or,
        BinaryOp::Eor => JitBinaryOp::Eor,
        BinaryOp::Cmp => JitBinaryOp::Cmp,
    }
}

#[inline]
fn jit_addr_op(op: AddrOp) -> JitAddrOp {
    match op {
        AddrOp::Adda => JitAddrOp::Adda,
        AddrOp::Suba => JitAddrOp::Suba,
        AddrOp::Cmpa => JitAddrOp::Cmpa,
    }
}

#[inline]
fn jit_bit_op(op: BitOp) -> JitBitOp {
    match op {
        BitOp::Test => JitBitOp::Test,
        BitOp::Change => JitBitOp::Change,
        BitOp::Clear => JitBitOp::Clear,
        BitOp::Set => JitBitOp::Set,
    }
}

/// Always inlined: a masked register load on the decoded-op fast path. Left
/// to the inliner it drops out of line whenever the surrounding interpreter
/// grows, and the call then costs more than the load.
#[inline(always)]
fn read_direct_reg(cpu: &CpuCore, reg: DirectReg, size: Size) -> u32 {
    match reg {
        DirectReg::Data(reg) => cpu.dar[reg as usize] & size.mask(),
        DirectReg::Addr(reg) => cpu.dar[8 + reg as usize] & size.mask(),
    }
}

/// Always inlined, like `read_direct_reg`: three operations on the hottest
/// decoded path.
#[inline(always)]
fn write_data_reg(cpu: &mut CpuCore, reg: usize, size: Size, value: u32) {
    let mask = size.mask();
    cpu.dar[reg] = (cpu.dar[reg] & !mask) | (value & mask);
}

#[inline]
pub(crate) fn is_pre_68020(cpu_type: CpuType) -> bool {
    matches!(
        cpu_type,
        CpuType::M68000 | CpuType::M68010 | CpuType::SCC68070
    )
}
