//! 68020+ long multiply/divide instructions (MULU.L/MULS.L, DIVU.L/DIVS.L and remainder forms).
//!
//! This matches the Musashi mc68040 fixture programs (`mul_long.s`, `divu_long.s`, `divs_long.s`).

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    /// DIVU.L / DIVS.L / DIVUL.L / DIVSL.L family.
    ///
    /// Opcode word: 0x4C40..0x4C7F (EA in low 6 bits)
    /// Extension word (Musashi-style, as observed in fixtures):
    /// - bit 11 (0x0800): signed (DIVS) when set, unsigned (DIVU) when clear
    /// - bit 10 (0x0400): 64-bit dividend when set (hi part in remainder reg)
    /// - bits 14..12: quotient destination register (Dq)
    /// - bits 2..0: remainder register (Dr) (also holds hi dividend when 0x0400 set)
    pub fn exec_divl<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let ext = self.read_imm_16(bus);

        let signed = (ext & 0x0800) != 0;
        let use_64 = (ext & 0x0400) != 0;
        // The 64/32 form was dropped from 68060 silicon; trap before the
        // divisor EA is touched (no zero-divide evaluation either).
        if use_64 && self.trap_unimpl_060() {
            return self.take_exception(bus, crate::core::exceptions::vector::UNIMPLEMENTED_INTEGER);
        }
        let dq = ((ext >> 12) & 7) as usize;
        let dr = (ext & 7) as usize;

        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return self.take_exception(bus, 4),
        };

        let divisor_u32 = self.read_ea(bus, mode, Size::Long);
        if divisor_u32 == 0 {
            return self.exception_zero_divide(bus);
        }

        let (quot_u32, rem_u32, overflow) = if signed {
            let divisor = divisor_u32 as i32 as i64;
            let dividend = if use_64 {
                let hi = self.d(dr) as i32 as i64;
                let lo = self.d(dq) as i64;
                (hi << 32) | lo
            } else {
                self.d(dq) as i32 as i64
            };
            if dividend == i64::MIN && divisor == -1 {
                // The one quotient that does not fit i64: hardware flags
                // overflow (Rust's division would panic on it).
                (0, 0, true)
            } else {
                let q = dividend / divisor;
                let r = dividend % divisor;
                let overflow = q < i32::MIN as i64 || q > i32::MAX as i64;
                (q as i32 as u32, r as i32 as u32, overflow)
            }
        } else {
            let divisor = divisor_u32 as u64;
            let dividend = if use_64 {
                ((self.d(dr) as u64) << 32) | (self.d(dq) as u64)
            } else {
                self.d(dq) as u64
            };
            let q = dividend / divisor;
            let r = dividend % divisor;
            let overflow = q > u32::MAX as u64;
            (q as u32, r as u32, overflow)
        };

        if overflow {
            // Overflow: V set, other flags undefined (we follow Musashi-ish: clear C, leave N/Z as-is).
            self.v_flag = 0x80;
            self.c_flag = 0;
            return 40;
        }

        // Write results: remainder first, then quotient, so the quotient
        // wins when Dr == Dq (the hardware write order).
        self.set_d(dr, rem_u32);
        self.set_d(dq, quot_u32);

        // Flags: Z/N from quotient, V=0, C=0. X unaffected.
        self.not_z_flag = quot_u32;
        self.n_flag = if (quot_u32 & 0x8000_0000) != 0 {
            0x80
        } else {
            0
        };
        self.v_flag = 0;
        self.c_flag = 0;

        40
    }

    /// Register write order for the 64-bit MUL result: the 68040/060 write
    /// Dh before Dl, so Dl survives a Dh == Dl collision; the 020/030 write
    /// in the reverse order.
    fn mull_high_written_first(&self) -> bool {
        matches!(
            self.cpu_type,
            crate::core::types::CpuType::M68EC040
                | crate::core::types::CpuType::M68LC040
                | crate::core::types::CpuType::M68040
                | crate::core::types::CpuType::M68060
        )
    }

    /// MULU.L / MULS.L family.
    ///
    /// Opcode word: 0x4C00..0x4C3F (EA in low 6 bits)
    /// Extension word (Musashi-style, as observed in fixtures):
    /// - bit 11 (0x0800): signed (MULS) when set, unsigned (MULU) when clear
    /// - bit 10 (0x0400): 64-bit result when set (high in Dh, low in Dl)
    /// - bits 14..12: low (and primary) destination register Dl
    /// - bits 2..0: high destination register Dh (only used when 0x0400 set)
    pub fn exec_mull<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let ext = self.read_imm_16(bus);

        let signed = (ext & 0x0800) != 0;
        let wide = (ext & 0x0400) != 0;
        // The 64-bit-result form was dropped from 68060 silicon (the 32-bit
        // form stays native). The consumed extension word is harmless: the
        // trap stacks the instruction address and the handler re-decodes.
        if wide && self.trap_unimpl_060() {
            return self.take_exception(bus, crate::core::exceptions::vector::UNIMPLEMENTED_INTEGER);
        }
        let dl = ((ext >> 12) & 7) as usize;
        let dh = (ext & 7) as usize;

        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return self.take_exception(bus, 4),
        };

        let src = self.read_ea(bus, mode, Size::Long);
        let dst = self.d(dl);

        if signed {
            let a = dst as i32 as i64;
            let b = src as i32 as i64;
            let prod = a.wrapping_mul(b);
            let lo = prod as i32 as u32;
            let hi = (prod >> 32) as i32 as u32;

            if wide {
                // The register write order flips per generation and decides
                // which half survives when Dh == Dl: 020/030 write Dl then
                // Dh, the 68040+ write Dh then Dl.
                if self.mull_high_written_first() {
                    self.set_d(dh, hi);
                    self.set_d(dl, lo);
                } else {
                    self.set_d(dl, lo);
                    self.set_d(dh, hi);
                }
                // Z/N reflect the full 64-bit product; V/C cleared.
                self.not_z_flag = lo | hi;
                self.n_flag = if (hi & 0x8000_0000) != 0 { 0x80 } else { 0 };
                self.v_flag = 0;
                self.c_flag = 0;
                return 40;
            }

            self.set_d(dl, lo);
            self.not_z_flag = lo;
            self.n_flag = if (lo & 0x8000_0000) != 0 { 0x80 } else { 0 };
            // Overflow if high is not sign-extension of low.
            let sign_ext = if (lo & 0x8000_0000) != 0 {
                0xFFFF_FFFF
            } else {
                0
            };
            self.v_flag = if hi != sign_ext { 0x80 } else { 0 };
            self.c_flag = 0;
            40
        } else {
            let prod = (dst as u64).wrapping_mul(src as u64);
            let lo = prod as u32;
            let hi = (prod >> 32) as u32;

            if wide {
                if self.mull_high_written_first() {
                    self.set_d(dh, hi);
                    self.set_d(dl, lo);
                } else {
                    self.set_d(dl, lo);
                    self.set_d(dh, hi);
                }
                self.not_z_flag = lo | hi;
                self.n_flag = if (hi & 0x8000_0000) != 0 { 0x80 } else { 0 };
                self.v_flag = 0;
                self.c_flag = 0;
                return 40;
            }

            self.set_d(dl, lo);
            self.not_z_flag = lo;
            self.n_flag = if (lo & 0x8000_0000) != 0 { 0x80 } else { 0 };
            // Overflow if high part non-zero.
            self.v_flag = if hi != 0 { 0x80 } else { 0 };
            self.c_flag = 0;
            40
        }
    }
}
