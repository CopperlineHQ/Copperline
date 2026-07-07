//! Integer arithmetic instructions.
//!
//! ADD, ADDA, ADDQ, ADDX, SUB, SUBA, SUBQ, SUBX, CMP, CMPA, NEG, NEGX, CLR, EXT

use crate::core::cpu::{CFLAG_SET, CpuCore, VFLAG_SET};
use crate::core::ea::AddressingMode;
use crate::core::execute::RUN_MODE_BERR_AERR_RESET;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    /// Execute ADD instruction.
    ///
    /// ADD <ea>, Dn  or  ADD Dn, <ea>
    pub fn exec_add<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst: u32,
    ) -> (u32, i32) {
        let result = src.wrapping_add(dst);
        self.set_add_flags(src, dst, result, size);
        (result & size.mask(), 4)
    }

    /// Execute ADDA instruction (no flags).
    pub fn exec_adda<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst_reg: usize,
    ) -> i32 {
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };
        let dst = self.a(dst_reg);
        self.set_a(dst_reg, dst.wrapping_add(src));
        8
    }

    /// Execute ADDQ instruction.
    ///
    /// ADDQ #<data>, <ea>
    pub fn exec_addq<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        data: u32,
        mode: AddressingMode,
    ) -> i32 {
        // For address register, no flags affected
        if let AddressingMode::AddressDirect(reg) = mode {
            let reg = reg as usize;
            self.set_a(reg, self.a(reg).wrapping_add(data));
            return 4;
        }

        // Resolve EA once: postinc/predec have side effects and must not be applied twice.
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        let (result, _) = self.exec_add::<B>(bus, size, data, dst);
        // ADDQ to memory polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        4
    }

    /// Execute ADDX instruction.
    pub fn exec_addx(&mut self, size: Size, src: u32, dst: u32) -> u32 {
        let mask = size.mask();
        let msb = size.msb_mask();
        let extend = if self.x_flag != 0 { 1u64 } else { 0u64 };

        let d = (dst & mask) as u64;
        let s = (src & mask) as u64;
        let sum = d + s + extend;
        let r = (sum as u32) & mask;

        // Flags (Z only cleared, never set)
        self.n_flag = if (r & msb) != 0 { 0x80 } else { 0 };
        if r != 0 {
            self.not_z_flag = r;
        }
        let v = (src ^ r) & (dst ^ r) & msb;
        self.v_flag = if v != 0 { VFLAG_SET } else { 0 };

        let carry = sum > (mask as u64);
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;

        r
    }

    /// Execute SUB instruction.
    pub fn exec_sub<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst: u32,
    ) -> (u32, i32) {
        let result = dst.wrapping_sub(src);
        self.set_sub_flags(src, dst, result, size);
        (result & size.mask(), 4)
    }

    /// Execute SUBA instruction (no flags).
    pub fn exec_suba<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst_reg: usize,
    ) -> i32 {
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };
        let dst = self.a(dst_reg);
        self.set_a(dst_reg, dst.wrapping_sub(src));
        8
    }

    /// Execute SUBQ instruction.
    pub fn exec_subq<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        data: u32,
        mode: AddressingMode,
    ) -> i32 {
        // For address register, no flags affected
        if let AddressingMode::AddressDirect(reg) = mode {
            let reg = reg as usize;
            self.set_a(reg, self.a(reg).wrapping_sub(data));
            return 4;
        }

        // Resolve EA once: postinc/predec have side effects and must not be applied twice.
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        let (result, _) = self.exec_sub::<B>(bus, size, data, dst);
        // SUBQ to memory polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        4
    }

    /// Execute SUBX instruction.
    pub fn exec_subx(&mut self, size: Size, src: u32, dst: u32) -> u32 {
        let mask = size.mask();
        let msb = size.msb_mask();
        let extend = if self.x_flag != 0 { 1u64 } else { 0u64 };

        let d = (dst & mask) as u64;
        let s = (src & mask) as u64;
        let sub = s + extend; // may be (mask+1) when src==mask and X==1
        let r = ((d.wrapping_sub(sub)) as u32) & mask;

        // Flags (Z only cleared, never set)
        self.n_flag = if (r & msb) != 0 { 0x80 } else { 0 };
        if r != 0 {
            self.not_z_flag = r;
        }
        // Overflow for SUBX/NEGX uses the original src operand (not src+X).
        let src_masked = src & mask;
        let dst_masked = dst & mask;
        self.v_flag = if ((src_masked ^ dst_masked) & (r ^ dst_masked) & msb) != 0 {
            VFLAG_SET
        } else {
            0
        };

        let borrow = sub > d;
        self.c_flag = if borrow { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;

        r
    }

    /// Execute CMP instruction.
    pub fn exec_cmp(&mut self, size: Size, src: u32, dst: u32) -> i32 {
        let result = dst.wrapping_sub(src);
        self.set_cmp_flags(src, dst, result, size);
        4
    }

    /// Execute CMPA instruction.
    pub fn exec_cmpa(&mut self, size: Size, src: u32, dst_reg: usize) -> i32 {
        let src = if size == Size::Word {
            src as i16 as i32 as u32
        } else {
            src
        };
        let dst = self.a(dst_reg);
        let result = dst.wrapping_sub(src);
        self.set_cmp_flags(src, dst, result, Size::Long);
        6
    }

    /// Execute CLR instruction.
    pub fn exec_clr<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let ea = self.resolve_ea(bus, mode, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        // 68000 quirk: CLR reads its destination operand before writing zero
        // (the 68010+ removed the read).
        if self.cpu_type == crate::core::types::CpuType::M68000 {
            let _ = self.read_resolved_ea(bus, ea, size);
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                return 50;
            }
        }
        // CLR polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, 0);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        self.n_flag = 0;
        self.not_z_flag = 0;
        self.v_flag = 0;
        self.c_flag = 0;
        4
    }

    /// Execute NEG instruction.
    pub fn exec_neg<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let ea = self.resolve_ea(bus, mode, size);
        let src = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        let result = 0u32.wrapping_sub(src);

        // NEG polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        self.set_sub_flags(src, 0, result, size);
        4
    }

    /// Execute NEGX instruction.
    pub fn exec_negx<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let ea = self.resolve_ea(bus, mode, size);
        let src = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        let result = self.exec_subx(size, src, 0);
        // NEGX polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        4
    }

    /// Execute NOT instruction.
    pub fn exec_not<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let ea = self.resolve_ea(bus, mode, size);
        let src = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        let result = !src & size.mask();

        // NOT polls IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        self.set_logic_flags(result, size);
        4
    }

    /// Execute EXT instruction.
    pub fn exec_ext(&mut self, size: Size, reg: usize) -> i32 {
        let value = self.d(reg);
        let result = match size {
            Size::Word => (value as i8 as i16 as u16) as u32 | (value & 0xFFFF0000),
            Size::Long => value as i16 as i32 as u32,
            Size::Byte => value, // Invalid
        };
        self.set_d(reg, result);
        self.set_logic_flags(result, size);
        4
    }

    /// Execute EXTB.L instruction (68020+).
    /// Sign-extends the low byte to a 32-bit long.
    pub fn exec_extb(&mut self, reg: usize) -> i32 {
        let value = self.d(reg);
        let result = value as i8 as i32 as u32;
        self.set_d(reg, result);
        self.set_logic_flags(result, Size::Long);
        4
    }

    /// Execute TST instruction.
    pub fn exec_tst<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let ea = self.resolve_ea(bus, mode, size);
        let value = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        self.set_logic_flags(value, size);
        4
    }

    /// Set flags for ADD operation.
    /// Musashi uses: CFLAG_8(res) = res (bit 8), CFLAG_16(res) = res>>8 (bit 16)
    /// For 32-bit: CFLAG_ADD_32(S, D, R) = ((S & D) | (~R & (S | D))) >> 23
    pub fn set_add_flags(&mut self, src: u32, dst: u32, result: u32, size: Size) {
        let msb = size.msb_mask();
        let mask = size.mask();

        // N flag: set if MSB of result is set
        self.n_flag = if result & msb != 0 { 0x80 } else { 0 };

        // Z flag: set if result (masked) is zero
        self.not_z_flag = result & mask;

        // V flag: overflow if both operands have same sign and result has different sign
        // Musashi: VFLAG_ADD = (src ^ result) & (dst ^ result)
        let v = (src ^ result) & (dst ^ result) & msb;
        self.v_flag = if v != 0 { 0x80 } else { 0 };

        // C flag: carry out of the MSB
        // For 32-bit, we need special handling since we can't get bit 32
        // Musashi: CFLAG_ADD_32 = ((S & D) | (~R & (S | D))) >> 23
        let carry = match size {
            Size::Byte => result & 0x100 != 0,
            Size::Word => result & 0x10000 != 0,
            Size::Long => {
                // Can't get bit 32, use Musashi's formula
                let s = src;
                let d = dst;
                let r = result;
                ((s & d) | (!r & (s | d))) & 0x80000000 != 0
            }
        };
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;
    }

    /// Set flags for ADDX operation (Z only cleared if non-zero).
    #[allow(dead_code)]
    fn set_addx_flags(&mut self, src: u32, dst: u32, result: u32, size: Size) {
        let msb = size.msb_mask();
        let mask = size.mask();
        let r = result & mask;
        let s = src & mask;
        let d = dst & mask;

        self.n_flag = if r & msb != 0 { 0x80 } else { 0 };
        // Z is only cleared, never set
        if r != 0 {
            self.not_z_flag = r;
        }

        let overflow = ((s ^ r) & (d ^ r) & msb) != 0;
        self.v_flag = if overflow { VFLAG_SET } else { 0 };

        // C flag: use same formula as ADD - Musashi CFLAG_ADD_32
        let carry = match size {
            Size::Byte => result & 0x100 != 0,
            Size::Word => result & 0x10000 != 0,
            Size::Long => {
                // Musashi: CFLAG_ADD_32 = ((S & D) | (~R & (S | D))) >> 23
                ((s & d) | (!r & (s | d))) & 0x80000000 != 0
            }
        };
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;
    }

    /// Set flags for SUB/CMP operation.
    /// Musashi: CFLAG_SUB_32(S, D, R) = ((S & R) | (~D & (S | R))) >> 23
    pub fn set_sub_flags(&mut self, src: u32, dst: u32, result: u32, size: Size) {
        let msb = size.msb_mask();
        let mask = size.mask();
        let r = result & mask;
        let s = src & mask;
        let d = dst & mask;

        // N flag: set if MSB of result is set
        self.n_flag = if r & msb != 0 { 0x80 } else { 0 };

        // Z flag: set if result (masked) is zero
        self.not_z_flag = r;

        // V flag: overflow if operands have different signs, result has different sign from dst
        // Musashi: VFLAG_SUB = (src ^ dst) & (result ^ dst)
        let v = (s ^ d) & (r ^ d) & msb;
        self.v_flag = if v != 0 { 0x80 } else { 0 };

        // C flag: borrow out of the MSB
        // For SUB, borrow if src > dst (unsigned)
        // Musashi: CFLAG_SUB_32 = ((S & R) | (~D & (S | R))) >> 23
        let carry = match size {
            Size::Byte => s > d,
            Size::Word => s > d,
            Size::Long => {
                // Use Musashi's formula for 32-bit
                ((s & r) | (!d & (s | r))) & 0x80000000 != 0
            }
        };
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;
    }

    /// Set flags for CMP (no X flag).
    fn set_cmp_flags(&mut self, src: u32, dst: u32, result: u32, size: Size) {
        let msb = size.msb_mask();
        let mask = size.mask();
        let r = result & mask;
        let s = src & mask;
        let d = dst & mask;

        self.n_flag = if r & msb != 0 { 0x80 } else { 0 };
        self.not_z_flag = r;

        let overflow = ((s ^ d) & (r ^ d) & msb) != 0;
        self.v_flag = if overflow { VFLAG_SET } else { 0 };

        let carry = s > d;
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        // X flag not affected by CMP
    }

    /// Set flags for SUBX operation.
    #[allow(dead_code)]
    fn set_subx_flags(&mut self, src: u32, dst: u32, result: u32, size: Size) {
        let msb = size.msb_mask();
        let mask = size.mask();
        let r = result & mask;
        let s = src & mask;
        let d = dst & mask;

        self.n_flag = if r & msb != 0 { 0x80 } else { 0 };
        // Z is only cleared, never set
        if r != 0 {
            self.not_z_flag = r;
        }

        let overflow = ((s ^ d) & (r ^ d) & msb) != 0;
        self.v_flag = if overflow { VFLAG_SET } else { 0 };

        // C flag: use same formula as SUB - Musashi CFLAG_SUB_32
        let carry = match size {
            Size::Byte => s > d,
            Size::Word => s > d,
            Size::Long => {
                // Musashi: CFLAG_SUB_32 = ((S & R) | (~D & (S | R))) >> 23
                ((s & r) | (!d & (s | r))) & 0x80000000 != 0
            }
        };
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        self.x_flag = self.c_flag;
    }

    /// Execute CHK instruction (word/long).
    ///
    /// Traps if Dn < 0 or Dn > bound.
    pub fn exec_chk<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        bound: u32,
        reg: usize,
    ) -> i32 {
        let (val, limit) = match size {
            Size::Word => (self.d(reg) as i16 as i32, bound as i16 as i32),
            Size::Long => (self.d(reg) as i32, bound as i32),
            Size::Byte => (self.d(reg) as i8 as i32, bound as i8 as i32),
        };

        // Flags: N reflects sign of Dn, Z cleared, V/C cleared
        self.n_flag = if val < 0 { 0x80 } else { 0 };
        self.not_z_flag = 1; // Z cleared
        self.v_flag = 0;
        self.c_flag = 0;

        if val < 0 || val > limit {
            // The failed comparison plus exception decision precede the
            // first stack write: 8 internal clocks for trap-on-too-big,
            // 10 for trap-on-negative (the upper-bound check runs first).
            self.internal_cycles(if val < 0 { 10 } else { 8 });
            return self.exception_chk(bus);
        }

        // Successful CHK: the bounds comparison spends 6 internal clocks
        // before the final prefetch.
        self.internal_cycles(6);
        10
    }
}
