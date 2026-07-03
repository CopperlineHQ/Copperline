//! BCD (Binary Coded Decimal) instructions.
//!
//! ABCD, SBCD, NBCD

use crate::core::cpu::{CFLAG_SET, CpuCore, NFLAG_SET, XFLAG_SET};
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

impl CpuCore {
    /// Execute ABCD register-to-register.
    ///
    /// ABCD Dy, Dx
    pub fn exec_abcd_rr(&mut self, src_reg: usize, dst_reg: usize) -> i32 {
        let src = self.d(src_reg) & 0xFF;
        let dst = self.d(dst_reg) & 0xFF;
        let result = self.bcd_add(src, dst);

        self.set_d(dst_reg, (self.d(dst_reg) & 0xFFFFFF00) | result);
        6
    }

    /// Execute ABCD memory-to-memory.
    ///
    /// ABCD -(Ay), -(Ax)
    pub fn exec_abcd_mm<B: AddressBus>(
        &mut self,
        bus: &mut B,
        src_reg: usize,
        dst_reg: usize,
    ) -> i32 {
        // Pre-decrement both. The address computation costs 2 internal clocks
        // before the first operand read.
        self.internal_cycles(2);
        let src_dec = if src_reg == 7 { 2 } else { 1 };
        let src_addr = self.a(src_reg).wrapping_sub(src_dec);
        self.set_a(src_reg, src_addr);
        let dst_dec = if dst_reg == 7 { 2 } else { 1 };
        let dst_addr = self.a(dst_reg).wrapping_sub(dst_dec);
        self.set_a(dst_reg, dst_addr);

        let src = self.read_8(bus, src_addr) as u32;
        let dst = self.read_8(bus, dst_addr) as u32;
        let result = self.bcd_add(src, dst);

        // 68000: the final prefetch precedes the destination writeback.
        self.top_up_prefetch(bus);
        self.write_8(bus, dst_addr, result as u8);
        18
    }

    /// Execute SBCD register-to-register.
    ///
    /// SBCD Dy, Dx
    pub fn exec_sbcd_rr(&mut self, src_reg: usize, dst_reg: usize) -> i32 {
        let src = self.d(src_reg) & 0xFF;
        let dst = self.d(dst_reg) & 0xFF;
        let result = self.bcd_sub(src, dst);

        self.set_d(dst_reg, (self.d(dst_reg) & 0xFFFFFF00) | result);
        6
    }

    /// Execute SBCD memory-to-memory.
    ///
    /// SBCD -(Ay), -(Ax)
    pub fn exec_sbcd_mm<B: AddressBus>(
        &mut self,
        bus: &mut B,
        src_reg: usize,
        dst_reg: usize,
    ) -> i32 {
        // Pre-decrement both. The address computation costs 2 internal clocks
        // before the first operand read.
        self.internal_cycles(2);
        let src_dec = if src_reg == 7 { 2 } else { 1 };
        let src_addr = self.a(src_reg).wrapping_sub(src_dec);
        self.set_a(src_reg, src_addr);
        let dst_dec = if dst_reg == 7 { 2 } else { 1 };
        let dst_addr = self.a(dst_reg).wrapping_sub(dst_dec);
        self.set_a(dst_reg, dst_addr);

        let src = self.read_8(bus, src_addr) as u32;
        let dst = self.read_8(bus, dst_addr) as u32;
        let result = self.bcd_sub(src, dst);

        // 68000: the final prefetch precedes the destination writeback.
        self.top_up_prefetch(bus);
        self.write_8(bus, dst_addr, result as u8);
        18
    }

    /// Execute NBCD (negate BCD).
    ///
    /// NBCD <ea>
    pub fn exec_nbcd<B: AddressBus>(&mut self, bus: &mut B, mode: AddressingMode) -> i32 {
        let is_reg = mode.is_register_direct();
        let ea = self.resolve_ea(bus, mode, Size::Byte);
        let dst = self.read_resolved_ea(bus, ea, Size::Byte);
        if self.sst_m68000_compat {
            // SingleStepTests/MAME fixtures treat NBCD as a BCD subtraction helper.
            let res = self.bcd_sub_sst(dst, 0);
            self.write_resolved_ea(bus, ea, Size::Byte, res);
            return if is_reg { 6 } else { 8 };
        }
        // Hardware model (WinUAE gencpu i_NBCD, cross-checked against real
        // 68000s by the cputest suite): 0 - dst - X with digit correction on
        // the low nibble when it underflows or exceeds 9, then a -0x60
        // correction/carry from the intermediate sum's upper bits.
        let x = if self.x_flag != 0 { 1u16 } else { 0 };
        let dst8 = (dst & 0xFF) as u16;

        let newv_lo = 0u16.wrapping_sub(dst8 & 0x0F).wrapping_sub(x);
        let newv_hi = 0u16.wrapping_sub(dst8 & 0xF0);
        let tmp_newv = newv_hi.wrapping_add(newv_lo);
        let corrected_lo = if newv_lo > 9 {
            newv_lo.wrapping_sub(6)
        } else {
            newv_lo
        };
        let mut newv = newv_hi.wrapping_add(corrected_lo);
        let carry = (newv & 0x1F0) > 0x90;
        if carry {
            newv = newv.wrapping_sub(0x60);
        }

        self.x_flag = if carry { XFLAG_SET } else { 0 };
        self.c_flag = if carry { CFLAG_SET } else { 0 };

        let res8 = (newv & 0xFF) as u32;
        self.bcd_set_nvz(res8, (tmp_newv & 0x80) != 0 && (newv & 0x80) == 0);
        self.write_resolved_ea(bus, ea, Size::Byte, res8);

        if is_reg { 6 } else { 8 }
    }

    // ========== BCD Helpers ==========

    /// SingleStepTests/MAME-style ABCD behavior (including "invalid digit" cases).
    fn bcd_add_sst(&mut self, src: u32, dst: u32) -> u32 {
        let x = if self.x_flag != 0 { 1u32 } else { 0 };
        let src = src & 0xFF;
        let dst = dst & 0xFF;

        let lo = (src & 0x0F).wrapping_add(dst & 0x0F).wrapping_add(x);
        let mut res = src.wrapping_add(dst).wrapping_add(x);
        if lo > 9 {
            res = res.wrapping_add(0x06);
        }
        // SingleStepTests behavior differs from Musashi: carry detection threshold is 0x9F.
        let carry = res > 0x9F;
        if carry {
            res = res.wrapping_add(0x60);
        }

        let res8 = res & 0xFF;
        self.x_flag = if carry { XFLAG_SET } else { 0 };
        self.c_flag = if carry { CFLAG_SET } else { 0 };
        if res8 != 0 {
            self.not_z_flag = res8;
        }
        res8
    }

    /// SingleStepTests/MAME-style SBCD behavior (including "invalid digit" cases).
    fn bcd_sub_sst(&mut self, src: u32, dst: u32) -> u32 {
        let x = if self.x_flag != 0 { 1i32 } else { 0i32 };
        let src = (src & 0xFF) as i32;
        let dst = (dst & 0xFF) as i32;

        let base = dst - src - x;
        let low_borrow = ((dst & 0x0F) - (src & 0x0F) - x) < 0;
        let borrow = base < 0;

        let mut res = base;
        if low_borrow {
            res -= 6;
        }
        let xc = res < 0 || borrow;
        if borrow {
            res -= 0x60;
        }

        let res8 = (res as u32) & 0xFF;
        self.x_flag = if xc { XFLAG_SET } else { 0 };
        self.c_flag = if xc { CFLAG_SET } else { 0 };
        if res8 != 0 {
            self.not_z_flag = res8;
        }
        res8
    }

    /// Undefined-flag policy for the BCD instructions, by CPU generation.
    ///
    /// The manuals leave N and V undefined; real hardware is deterministic:
    /// the 68000/010 derive both from the correction steps, the 68020/030
    /// derive N and clear V, and the 68040/060 leave N and V unchanged
    /// (WinUAE's xBCD_KEEPS_V_FLAG / xBCD_KEEPS_N_FLAG model). Z is sticky
    /// on every generation.
    fn bcd_nv_level(&self) -> u8 {
        match self.cpu_type {
            CpuType::Invalid | CpuType::M68000 | CpuType::M68010 | CpuType::SCC68070 => 0,
            CpuType::M68EC020 | CpuType::M68020 | CpuType::M68EC030 | CpuType::M68030 => 2,
            CpuType::M68EC040 | CpuType::M68LC040 | CpuType::M68040 | CpuType::M68060 => 4,
        }
    }

    /// Apply the per-generation N/V/Z policy for a BCD result byte.
    /// `v_set` is the 68000/010 V value derived by the caller.
    fn bcd_set_nvz(&mut self, res8: u32, v_set: bool) {
        // Z: cleared if the result is nonzero, unchanged otherwise.
        self.not_z_flag |= res8;
        let level = self.bcd_nv_level();
        if level < 4 {
            self.n_flag = if (res8 & 0x80) != 0 { NFLAG_SET } else { 0 };
            self.v_flag = if level < 2 && v_set { 0x80 } else { 0 };
        }
    }

    /// Perform BCD addition: src + dst + X
    fn bcd_add(&mut self, src: u32, dst: u32) -> u32 {
        if self.sst_m68000_compat {
            return self.bcd_add_sst(src, dst);
        }
        // Hardware model (WinUAE gencpu i_ABCD, cross-checked against real
        // 68000s by the cputest suite): the low-nibble +6 correction applies
        // when the digit sum exceeds 9, and the +0x60 correction/carry come
        // from the corrected sum's upper bits.
        let x = if self.x_flag != 0 { 1u16 } else { 0 };
        let src = src & 0xFF;
        let dst = dst & 0xFF;

        let newv_lo = ((src & 0x0F) + (dst & 0x0F)) as u16 + x;
        let newv_hi = ((src & 0xF0) + (dst & 0xF0)) as u16;
        let tmp_newv = newv_hi + newv_lo;
        let mut newv = tmp_newv;
        if newv_lo > 9 {
            newv += 6;
        }
        let carry = (newv & 0x3F0) > 0x90;
        if carry {
            newv += 0x60;
        }

        self.x_flag = if carry { XFLAG_SET } else { 0 };
        self.c_flag = if carry { CFLAG_SET } else { 0 };

        let res8 = (newv & 0xFF) as u32;
        self.bcd_set_nvz(res8, (tmp_newv & 0x80) == 0 && (newv & 0x80) != 0);
        res8
    }

    /// Perform BCD subtraction: dst - src - X
    fn bcd_sub(&mut self, src: u32, dst: u32) -> u32 {
        if self.sst_m68000_compat {
            return self.bcd_sub_sst(src, dst);
        }
        // Hardware model (WinUAE gencpu i_SBCD, cross-checked against real
        // 68000s by the cputest suite): the low-nibble -6 correction applies
        // only on a digit borrow (not for "invalid" digits A-F), and the
        // -0x60 correction follows the uncorrected byte-wide borrow.
        let x = if self.x_flag != 0 { 1u32 } else { 0 };
        let src = src & 0xFF;
        let dst = dst & 0xFF;

        let newv_lo = ((dst & 0x0F) as u16)
            .wrapping_sub((src & 0x0F) as u16)
            .wrapping_sub(x as u16);
        let newv_hi = ((dst & 0xF0) as u16).wrapping_sub((src & 0xF0) as u16);
        let tmp_newv = newv_hi.wrapping_add(newv_lo);
        let mut newv = tmp_newv;
        let mut bcd = 0u32;
        if newv_lo & 0xF0 != 0 {
            newv = newv.wrapping_sub(6);
            bcd = 6;
        }
        if dst.wrapping_sub(src).wrapping_sub(x) & 0x100 != 0 {
            newv = newv.wrapping_sub(0x60);
        }
        let carry = dst.wrapping_sub(src).wrapping_sub(bcd).wrapping_sub(x) & 0x300 != 0;

        self.x_flag = if carry { XFLAG_SET } else { 0 };
        self.c_flag = if carry { CFLAG_SET } else { 0 };

        let res8 = (newv & 0xFF) as u32;
        self.bcd_set_nvz(res8, (tmp_newv & 0x80) != 0 && (newv & 0x80) == 0);
        res8
    }

    // ========== PACK/UNPK (68020+) ==========

    /// Execute PACK register-to-register (68020+).
    ///
    /// PACK Ds, Dd, #adj
    /// The adjustment is added to the raw 16-bit source BEFORE the digits
    /// are packed: result = pack(src + adj).
    pub fn exec_pack_rr(&mut self, src_reg: usize, dst_reg: usize, adj: u16) -> i32 {
        let val = (self.d(src_reg) as u16).wrapping_add(adj);
        let packed = (((val >> 4) & 0xF0) | (val & 0x0F)) as u32;
        self.set_d(dst_reg, (self.d(dst_reg) & 0xFFFFFF00) | packed);
        6
    }

    /// Execute PACK memory-to-memory (68020+).
    ///
    /// PACK -(As), -(Ad), #adj
    pub fn exec_pack_mm<B: AddressBus>(
        &mut self,
        bus: &mut B,
        src_reg: usize,
        dst_reg: usize,
        adj: u16,
    ) -> i32 {
        // Read source word from predecrement
        let src_addr = self.a(src_reg).wrapping_sub(2);
        self.set_a(src_reg, src_addr);
        let val = self.read_16(bus, src_addr).wrapping_add(adj);

        let result = ((val >> 4) & 0xF0) as u8 | (val & 0x0F) as u8;

        // Write destination byte to predecrement (A7 stays word-aligned)
        let dst_dec = if dst_reg == 7 { 2 } else { 1 };
        let dst_addr = self.a(dst_reg).wrapping_sub(dst_dec);
        self.set_a(dst_reg, dst_addr);
        self.write_8(bus, dst_addr, result);
        13
    }

    /// Execute UNPK register-to-register (68020+).
    ///
    /// UNPK Ds, Dd, #adj
    /// Result = ((src[7:4] << 8) | src[3:0]) + adj
    pub fn exec_unpk_rr(&mut self, src_reg: usize, dst_reg: usize, adj: u16) -> i32 {
        let src = self.d(src_reg) & 0xFF;
        let unpacked = (((src >> 4) & 0xF) << 8) | (src & 0xF);
        let result = (unpacked + adj as u32) & 0xFFFF;
        self.set_d(dst_reg, (self.d(dst_reg) & 0xFFFF0000) | result);
        8
    }

    /// Execute UNPK memory-to-memory (68020+).
    ///
    /// UNPK -(As), -(Ad), #adj
    pub fn exec_unpk_mm<B: AddressBus>(
        &mut self,
        bus: &mut B,
        src_reg: usize,
        dst_reg: usize,
        adj: u16,
    ) -> i32 {
        // Read source byte from predecrement (A7 stays word-aligned)
        let src_dec = if src_reg == 7 { 2 } else { 1 };
        let src_addr = self.a(src_reg).wrapping_sub(src_dec);
        self.set_a(src_reg, src_addr);
        let src = self.read_8(bus, src_addr) as u32;

        let unpacked = (((src >> 4) & 0xF) << 8) | (src & 0xF);
        let result = ((unpacked + adj as u32) & 0xFFFF) as u16;

        // Write destination word to predecrement
        let dst_addr = self.a(dst_reg).wrapping_sub(2);
        self.set_a(dst_reg, dst_addr);
        self.write_16(bus, dst_addr, result);
        13
    }
}
