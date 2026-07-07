//! Bit manipulation instructions.
//!
//! BTST, BSET, BCLR, BCHG

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
    /// Execute BTST instruction.
    ///
    /// BTST Dn/<#data>, <ea>
    pub fn exec_btst<B: AddressBus>(
        &mut self,
        bus: &mut B,
        bit_num: u32,
        mode: AddressingMode,
    ) -> i32 {
        // Register operand: modulo 32, memory operand: modulo 8
        let (size, bit) = if mode.is_register_direct() {
            (Size::Long, bit_num & 31)
        } else {
            (Size::Byte, bit_num & 7)
        };

        let value = self.read_ea(bus, mode, size);
        self.not_z_flag = if value & (1 << bit) != 0 { 1 } else { 0 };

        if size == Size::Long { 6 } else { 4 }
    }

    /// Execute BSET instruction.
    ///
    /// BSET Dn/<#data>, <ea>
    pub fn exec_bset<B: AddressBus>(
        &mut self,
        bus: &mut B,
        bit_num: u32,
        mode: AddressingMode,
    ) -> i32 {
        let (size, bit) = if mode.is_register_direct() {
            (Size::Long, bit_num & 31)
        } else {
            (Size::Byte, bit_num & 7)
        };

        let ea = self.resolve_ea(bus, mode, size);
        let value = self.read_resolved_ea(bus, ea, size);
        self.not_z_flag = if value & (1 << bit) != 0 { 1 } else { 0 };
        let result = value | (1 << bit);
        // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());

        8
    }

    /// Execute BCLR instruction.
    ///
    /// BCLR Dn/<#data>, <ea>
    pub fn exec_bclr<B: AddressBus>(
        &mut self,
        bus: &mut B,
        bit_num: u32,
        mode: AddressingMode,
    ) -> i32 {
        let (size, bit) = if mode.is_register_direct() {
            (Size::Long, bit_num & 31)
        } else {
            (Size::Byte, bit_num & 7)
        };

        let ea = self.resolve_ea(bus, mode, size);
        let value = self.read_resolved_ea(bus, ea, size);
        self.not_z_flag = if value & (1 << bit) != 0 { 1 } else { 0 };
        let result = value & !(1 << bit);
        // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());

        if size == Size::Long { 10 } else { 8 }
    }

    /// Execute BCHG instruction.
    ///
    /// BCHG Dn/<#data>, <ea>
    pub fn exec_bchg<B: AddressBus>(
        &mut self,
        bus: &mut B,
        bit_num: u32,
        mode: AddressingMode,
    ) -> i32 {
        let (size, bit) = if mode.is_register_direct() {
            (Size::Long, bit_num & 31)
        } else {
            (Size::Byte, bit_num & 7)
        };

        let ea = self.resolve_ea(bus, mode, size);
        let value = self.read_resolved_ea(bus, ea, size);
        self.not_z_flag = if value & (1 << bit) != 0 { 1 } else { 0 };
        let result = value ^ (1 << bit);
        // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());

        8
    }

    /// Execute TAS instruction.
    ///
    /// TAS <ea>
    pub fn exec_tas<B: AddressBus>(&mut self, bus: &mut B, mode: AddressingMode) -> i32 {
        let ea = self.resolve_ea(bus, mode, Size::Byte);
        let value = self.read_resolved_ea(bus, ea, Size::Byte);
        self.set_logic_flags(value, Size::Byte);

        // Set bit 7
        let result = value | 0x80;
        if self.cpu_type == crate::core::types::CpuType::M68000 {
            if let crate::core::ea::EaResult::Memory(addr) = ea {
                // 68000 TAS bus order (Moira execTasEa): read, 2 internal
                // clocks, write, THEN the final prefetch -- unlike the
                // other RMW instructions, whose prefetch precedes the
                // writeback (the read-modify-write cycle is indivisible on
                // real hardware). The trailing prefetch carries the IPL
                // poll, which is the default last-access sample.
                self.internal_cycles(2);
                self.write_8(bus, addr, result as u8);
            } else {
                self.write_resolved_ea(bus, ea, Size::Byte, result);
            }
        } else {
            self.write_resolved_ea(bus, ea, Size::Byte, result);
        }

        self.trace_t0_68040_sync();
        4
    }
}
