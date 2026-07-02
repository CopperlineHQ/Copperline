//! 68020+ compare-and-swap instructions.
//!
//! Implements CAS and CAS2 (long-sized, as used by the mc68040 Musashi fixture set).

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    /// CAS.<size> Dc,Du,<ea>
    ///
    /// Musashi fixtures only use CAS.L (opcode pattern 0x0EC0..0x0EFF).
    pub fn exec_cas<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        // Extension word encodes Du and Dc.
        let ext = self.read_imm_16(bus);
        let du = ((ext >> 6) & 7) as usize;
        let dc = (ext & 7) as usize;

        // Effective address is in opcode.
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return self.take_exception(bus, 4),
        };
        if mode.is_register_direct() || matches!(mode, AddressingMode::Immediate) {
            return self.take_exception(bus, 4);
        }

        let size = decode_cas_size(opcode);
        // Resolve the operand address with post-increment/pre-decrement
        // commits deferred: a misaligned CAS was dropped from 68060 silicon
        // and must trap with no architectural state changed, so the 68060SP
        // handler can emulate the whole instruction.
        let (addr, ea_commit) = match mode {
            AddressingMode::PostIncrement(reg) => {
                let addr = self.a(reg as usize);
                let inc = if reg == 7 && size == Size::Byte {
                    2 // A7 always stays word-aligned
                } else {
                    size.bytes()
                };
                (addr, Some((reg as usize, addr.wrapping_add(inc))))
            }
            AddressingMode::PreDecrement(reg) => {
                let dec = if reg == 7 && size == Size::Byte {
                    2
                } else {
                    size.bytes()
                };
                let addr = self.a(reg as usize).wrapping_sub(dec);
                (addr, Some((reg as usize, addr)))
            }
            _ => (self.get_ea_address(bus, mode, size), None),
        };
        if size != Size::Byte && (addr & 1) != 0 && self.trap_unimpl_int_060() {
            return self
                .take_exception(bus, crate::core::exceptions::vector::UNIMPLEMENTED_INTEGER);
        }
        if let Some((reg, value)) = ea_commit {
            self.set_a(reg, value);
        }
        let mem = match size {
            Size::Byte => self.read_8(bus, addr) as u32,
            Size::Word => self.read_16(bus, addr) as u32,
            Size::Long => self.read_32(bus, addr),
        };
        let dc_val = self.d(dc);

        // Flags are set as if CMP.<size> mem, Dc (i.e. Dc - mem).
        self.exec_cmp(size, mem, dc_val);

        if (dc_val & size.mask()) == (mem & size.mask()) {
            // Compare succeeded: write update register to memory.
            let v = self.d(du) & size.mask();
            match size {
                Size::Byte => self.write_8(bus, addr, v as u8),
                Size::Word => self.write_16(bus, addr, v as u16),
                Size::Long => self.write_32(bus, addr, v),
            }
        } else {
            // Compare failed: load memory value into compare register.
            self.set_d(dc, write_d_sized(dc_val, mem, size));
        }

        20
    }

    /// CAS2.<size> Dc1:Dc2,Du1:Du2,(Rn1):(Rn2)
    ///
    /// Musashi fixtures use CAS2.L with two extension words.
    pub fn exec_cas2<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let ext1 = self.read_imm_16(bus);
        let ext2 = self.read_imm_16(bus);

        let (rn1, du1, dc1) = decode_cas2_ext(ext1);
        let (rn2, du2, dc2) = decode_cas2_ext(ext2);

        let addr1 = self.read_cas2_rn_address(rn1);
        let addr2 = self.read_cas2_rn_address(rn2);

        let size = decode_cas_size(opcode);
        let mem1 = match size {
            Size::Byte => self.read_8(bus, addr1) as u32,
            Size::Word => self.read_16(bus, addr1) as u32,
            Size::Long => self.read_32(bus, addr1),
        };
        let mem2 = match size {
            Size::Byte => self.read_8(bus, addr2) as u32,
            Size::Word => self.read_16(bus, addr2) as u32,
            Size::Long => self.read_32(bus, addr2),
        };

        let dc1_val = self.d(dc1);
        let dc2_val = self.d(dc2);

        // Compare first operand; if equal, compare second operand.
        self.exec_cmp(size, mem1, dc1_val);
        if (dc1_val & size.mask()) == (mem1 & size.mask()) {
            self.exec_cmp(size, mem2, dc2_val);
        }

        if (dc1_val & size.mask()) == (mem1 & size.mask()) && (dc2_val & size.mask()) == (mem2 & size.mask()) {
            // Both comparisons succeeded: swap in update registers.
            let v1 = self.d(du1) & size.mask();
            let v2 = self.d(du2) & size.mask();
            match size {
                Size::Byte => {
                    self.write_8(bus, addr1, v1 as u8);
                    self.write_8(bus, addr2, v2 as u8);
                }
                Size::Word => {
                    self.write_16(bus, addr1, v1 as u16);
                    self.write_16(bus, addr2, v2 as u16);
                }
                Size::Long => {
                    self.write_32(bus, addr1, v1);
                    self.write_32(bus, addr2, v2);
                }
            }
        } else {
            // Any mismatch: load both memory operands into compare registers.
            self.set_d(dc1, write_d_sized(dc1_val, mem1, size));
            self.set_d(dc2, write_d_sized(dc2_val, mem2, size));
        }

        40
    }

    fn read_cas2_rn_address(&self, rn: u8) -> u32 {
        if rn >= 8 {
            self.a((rn - 8) as usize)
        } else {
            // Not expected in our fixtures; treat as data register containing address.
            self.d(rn as usize)
        }
    }
}

#[inline]
fn decode_cas2_ext(ext: u16) -> (u8, usize, usize) {
    // Layout (as used by Musashi fixtures):
    // - bits 15..12: Rn (0..15; 8..15 == A0..A7)
    // - bits 8..6: Du
    // - bits 2..0: Dc
    let rn = ((ext >> 12) & 0xF) as u8;
    let du = ((ext >> 6) & 7) as usize;
    let dc = (ext & 7) as usize;
    (rn, du, dc)
}

#[inline]
fn decode_cas_size(opcode: u16) -> Size {
    match opcode & 0x0E00 {
        0x0A00 => Size::Byte,
        0x0C00 => Size::Word,
        0x0E00 => Size::Long,
        _ => Size::Long,
    }
}

#[inline]
fn write_d_sized(old: u32, value: u32, size: Size) -> u32 {
    match size {
        Size::Byte => (old & 0xFFFF_FF00) | (value & 0xFF),
        Size::Word => (old & 0xFFFF_0000) | (value & 0xFFFF),
        Size::Long => value,
    }
}


