//! 68020+ CMP2 / CHK2 (bounds compare / bounds check).
//!
//! Based on the Musashi mc68040 test suite sources in
//! `tests/fixtures/Musashi/test/mc68040/{cmp2,chk2}.s`.
//!
//! Encoding (as used by GNU as for 68040):
//!   opcode: 0000 0ss0 11 mmm rrr   (ss: 00=byte, 01=word, 10=long)
//!   ext word:
//!     - bit 11: 1 = CHK2 (may trap), 0 = CMP2
//!     - bits 15..12: register specifier (0..7 = D0..D7, 8..15 = A0..A7)
//!     - remaining bits unused in our fixtures
//!
//! Semantics (as required by the fixture):
//! - Reads a lower and upper bound from <ea> (two consecutive sized values).
//! - Compares the specified register value against the bounds.
//! - Sets C=1 when the value is out of range, else C=0.
//! - For CHK2, triggers CHK exception (vector 6) when out of range.
//!
//! Note: The Musashi fixture expects CMP2.B to behave using unsigned comparisons for bounds/operand.

use crate::core::cpu::{CFLAG_SET, CpuCore};
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    pub fn exec_cmp2_chk2<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let size = match (opcode >> 9) & 3 {
            0 => Size::Byte,
            1 => Size::Word,
            2 => Size::Long,
            _ => return self.take_exception(bus, 4),
        };

        let ext = self.read_imm_16(bus);
        let is_chk2 = (ext & 0x0800) != 0;
        let rn = ((ext >> 12) & 0xF) as u8;

        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return self.take_exception(bus, 4),
        };
        if matches!(mode, AddressingMode::Immediate) {
            return self.take_exception(bus, 4);
        }

        let addr = self.get_ea_address(bus, mode, size);

        // Load bounds (lower, upper) from memory, consecutive.
        let (lower_u, upper_u) = match size {
            Size::Byte => {
                let lo = self.read_8(bus, addr) as u32;
                let hi = self.read_8(bus, addr.wrapping_add(1)) as u32;
                (lo, hi)
            }
            Size::Word => {
                let lo = self.read_16(bus, addr) as u32;
                let hi = self.read_16(bus, addr.wrapping_add(2)) as u32;
                (lo, hi)
            }
            Size::Long => {
                let lo = self.read_32(bus, addr);
                let hi = self.read_32(bus, addr.wrapping_add(4));
                (lo, hi)
            }
        };

        // Fetch operand from specified register.
        let raw = if rn >= 8 {
            self.a((rn - 8) as usize)
        } else {
            self.d(rn as usize)
        };

        // Hardware model (WinUAE gencpu i_CHK2): everything is compared as
        // signed 32-bit values. The bounds are sign-extended for byte/word;
        // a data register operand is sign-extended too, but an address
        // register operand always compares its full 32-bit value.
        let (lower, upper, reg_val) = match size {
            Size::Byte => (
                lower_u as u8 as i8 as i32,
                upper_u as u8 as i8 as i32,
                if rn < 8 {
                    raw as u8 as i8 as i32
                } else {
                    raw as i32
                },
            ),
            Size::Word => (
                lower_u as u16 as i16 as i32,
                upper_u as u16 as i16 as i32,
                if rn < 8 {
                    raw as u16 as i16 as i32
                } else {
                    raw as i32
                },
            ),
            Size::Long => (lower_u as i32, upper_u as i32, raw as i32),
        };

        // Z is set when the operand equals either bound. C is set when the
        // operand is outside the bounds; reversed bounds (lower > upper)
        // select the wrapped range.
        let on_bound = reg_val == lower || reg_val == upper;
        let out_of_range = !on_bound
            && if lower <= upper {
                reg_val < lower || reg_val > upper
            } else {
                reg_val > upper && reg_val < lower
            };

        self.c_flag = if out_of_range { CFLAG_SET } else { 0 };
        self.v_flag = 0;
        self.not_z_flag = if on_bound { 0 } else { 1 };
        self.n_flag = 0;
        // X unaffected

        if is_chk2 && out_of_range {
            // CHK2 traps via CHK vector (6).
            return self.exception_chk(bus);
        }

        12
    }
}


