//! MOVE16 16-byte aligned block transfers (68040/68060).
//!
//! The five encodings cover `(Ay)+,(xxx).L`, `(xxx).L,(Ay)+`,
//! `(Ay),(xxx).L`, `(xxx).L,(Ay)`, and `(Ax)+,(Ay)+`. Both addresses are
//! forced to 16-byte alignment: the low four bits are ignored and do not
//! raise an address error.

use crate::core::cpu::CpuCore;
use crate::core::memory::AddressBus;

impl CpuCore {
    /// MOVE16 - 16-byte aligned block transfer (68040/68060).
    pub fn exec_move16<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let reg = (opcode & 7) as usize;
        // Registers to post-increment after the transfer.
        let mut inc: [Option<usize>; 2] = [None, None];

        let (src_addr, dst_addr) = match opcode & 0xFFF8 {
            0xF620 => {
                // (Ax)+,(Ay)+: destination register in the extension word.
                // When Ax == Ay the register is incremented only once.
                let ext = self.read_imm_16(bus);
                let dst_reg = ((ext >> 12) & 7) as usize;
                if reg != dst_reg {
                    inc[0] = Some(reg);
                }
                inc[1] = Some(dst_reg);
                (self.a(reg) & !15, self.a(dst_reg) & !15)
            }
            0xF600 => {
                let abs = self.read_imm_32(bus);
                inc[0] = Some(reg);
                (self.a(reg) & !15, abs & !15)
            }
            0xF608 => {
                let abs = self.read_imm_32(bus);
                inc[1] = Some(reg);
                (abs & !15, self.a(reg) & !15)
            }
            0xF610 => {
                let abs = self.read_imm_32(bus);
                (self.a(reg) & !15, abs & !15)
            }
            0xF618 => {
                let abs = self.read_imm_32(bus);
                (abs & !15, self.a(reg) & !15)
            }
            _ => return 0,
        };

        // Line-fill semantics: all four reads complete before the writes.
        let mut v = [0u32; 4];
        for (i, slot) in v.iter_mut().enumerate() {
            *slot = self.read_32(bus, src_addr + (i as u32) * 4);
        }
        for (i, value) in v.iter().enumerate() {
            self.write_32(bus, dst_addr + (i as u32) * 4, *value);
        }

        for r in inc.into_iter().flatten() {
            self.set_a(r, self.a(r).wrapping_add(16));
        }

        // Condition codes are not affected
        4
    }
}
