// MOVES - Move to/from Address Space (68010+)
//
// Opcode: 0000 1110 ssmm mrrr
// Extension word: 1aaa rrrd 0000 0000
//   a/rrr = register number (0-7 for Dn, A for An)
//   d = direction (0 = register to EA, 1 = EA to register)

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    /// MOVES - Move to/from address space using SFC/DFC.
    /// The Amiga bus does not decode function codes, so the access itself is
    /// a normal memory access -- but with the PMMU enabled the address space
    /// matters: the data cycle translates under SFC/DFC (mmu_fc_override),
    /// which selects the 030 FCL table branch / TTR matches and the 040
    /// user-vs-supervisor root pointer.
    pub fn exec_moves<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        // Check supervisor mode
        if self.s_flag == 0 {
            return self.take_exception(bus, 8); // Privilege violation
        }

        let size = match (opcode >> 6) & 3 {
            0 => Size::Byte,
            1 => Size::Word,
            2 => Size::Long,
            _ => return 0, // Invalid
        };

        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        let ext = self.read_imm_16(bus);
        let reg_num = ((ext >> 12) & 0xF) as usize;
        let is_areg = reg_num >= 8;
        let reg_idx = reg_num & 7;
        let direction = (ext >> 11) & 1; // 0 = EA to reg, 1 = reg to EA

        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return 0,
        };

        // For MOVES An,(An)+ / MOVES An,-(An) the stored value depends on
        // whether the EA register update happens before the source register
        // is captured. Real hardware (WinUAE cputest model): the 68020-040
        // store the updated An, and so does the 68010 for byte/word; the
        // 68010 long form and the 68060 store the original value.
        let source_before_ea = if is_areg { Some(self.a(reg_idx)) } else { None };

        let addr = self.get_ea_address(bus, mode, size);

        // The 68010 spends mode-dependent internal clocks between the EA
        // calculation and the data cycle (Moira execMoves: SYNC 6 for (An)
        // and d8(An,Xn), 8 for (An)+, 6 for -(An), 4 for d16(An) and
        // absolute modes), then performs the access and the final prefetch
        // (which carries the IPL poll, the default last-access sample).
        // The core previously billed none of this, running MOVES ~6-8
        // clocks fast and shifting every later bus access early.
        let moves_sync: u32 = match mode {
            AddressingMode::AddressIndirect(_) | AddressingMode::PreDecrement(_) => 6,
            AddressingMode::PostIncrement(_) => 8,
            AddressingMode::Index(_) => 6,
            AddressingMode::Displacement(_)
            | AddressingMode::AbsoluteShort
            | AddressingMode::AbsoluteLong => 4,
            _ => 0,
        };
        if self.prefetch_enabled() {
            self.internal_cycles(moves_sync);
        }

        if direction == 1 {
            // Register to EA (write): the data cycle runs in the DFC space.
            let ea_updates_register = matches!(
                mode,
                AddressingMode::PostIncrement(_) | AddressingMode::PreDecrement(_)
            );
            let stores_original = match self.cpu_type {
                crate::core::types::CpuType::M68060 => true,
                crate::core::types::CpuType::M68010 | crate::core::types::CpuType::SCC68070 => {
                    size == Size::Long
                }
                _ => false,
            };
            let value = if is_areg && ea_updates_register && stores_original {
                source_before_ea.unwrap()
            } else if is_areg {
                self.a(reg_idx)
            } else {
                self.d(reg_idx)
            };

            self.mmu_fc_override = Some((self.dfc & 7) as u8);
            match size {
                Size::Byte => self.write_8(bus, addr, value as u8),
                Size::Word => self.write_16(bus, addr, value as u16),
                Size::Long => self.write_32(bus, addr, value),
            }
            self.mmu_fc_override = None;
        } else {
            // EA to register (read): the data cycle runs in the SFC space.
            self.mmu_fc_override = Some((self.sfc & 7) as u8);
            let value = match size {
                Size::Byte => self.read_8(bus, addr) as u32,
                Size::Word => self.read_16(bus, addr) as u32,
                Size::Long => self.read_32(bus, addr),
            };
            self.mmu_fc_override = None;

            if is_areg {
                // Sign extend for address register
                let extended = match size {
                    Size::Byte => value as i8 as i32 as u32,
                    Size::Word => value as i16 as i32 as u32,
                    Size::Long => value,
                };
                self.set_a(reg_idx, extended);
            } else {
                match size {
                    Size::Byte => self.set_d(reg_idx, (self.d(reg_idx) & 0xFFFFFF00) | value),
                    Size::Word => self.set_d(reg_idx, (self.d(reg_idx) & 0xFFFF0000) | value),
                    Size::Long => self.set_d(reg_idx, value),
                }
            }
        }

        // Condition codes are not affected
        self.trace_t0_68040_sync();
        if self.prefetch_enabled() {
            // Billed total on the 68010: extension-word prefetch + the EA
            // extension fetches + the internal clocks above + the data
            // cycle + the final prefetch.
            // ea_calc_cycles also counts the 2 internal clocks the -(An)
            // and d8(An,Xn) address calculations bill inside resolve_ea
            // (Moira computeEA SYNC(2)), putting the -(An) byte/word total
            // at 20 and d8(An,Xn) at 24.
            let access: i32 = if size == Size::Long { 8 } else { 4 };
            4 + self.ea_calc_cycles(mode) + moves_sync as i32 + access + 4
        } else {
            4
        }
    }
}
