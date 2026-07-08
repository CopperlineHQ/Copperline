//! Bit manipulation instructions.
//!
//! BTST, BSET, BCLR, BCHG

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

impl CpuCore {
    fn finish_m68000_register_bitop<B: AddressBus>(
        &mut self,
        bus: &mut B,
        internal_clocks: u32,
        poll_ipl: bool,
    ) {
        if self.cpu_type != CpuType::M68000 {
            return;
        }

        // 68000 register-destination bit ops finish with the final prefetch,
        // optional IPL poll on that prefetch, then the remaining internal
        // clocks before the instruction boundary.
        self.top_up_prefetch(bus);
        if poll_ipl {
            self.ipl_poll_point(bus);
        }
        self.internal_cycles(internal_clocks);
        self.flush_sync(bus);
    }

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

        if size == Size::Long {
            self.finish_m68000_register_bitop(bus, 2, false);
        }

        if size == Size::Long {
            6
        } else {
            4
        }
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
        if self.cpu_type == CpuType::M68000 && size == Size::Long {
            self.finish_m68000_register_bitop(bus, if bit > 15 { 4 } else { 2 }, true);
            self.write_resolved_ea(bus, ea, size, result & size.mask());
        } else {
            // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
            self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());
        }

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
        if self.cpu_type == CpuType::M68000 && size == Size::Long {
            self.finish_m68000_register_bitop(bus, if bit > 15 { 6 } else { 4 }, true);
            self.write_resolved_ea(bus, ea, size, result & size.mask());
        } else {
            // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
            self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());
        }

        if size == Size::Long {
            10
        } else {
            8
        }
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
        if self.cpu_type == CpuType::M68000 && size == Size::Long {
            self.finish_m68000_register_bitop(bus, if bit > 15 { 4 } else { 2 }, true);
            self.write_resolved_ea(bus, ea, size, result & size.mask());
        } else {
            // BCHG/BCLR/BSET poll IPL during the pre-writeback prefetch.
            self.write_resolved_ea_np_poll(bus, ea, size, result & size.mask());
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ea::AddressingMode;
    use crate::core::memory::AddressBus;
    use crate::core::types::CpuType;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        ReadWord(u32),
        Sync(u32),
        IplHold,
    }

    #[derive(Default)]
    struct TraceBus {
        events: Vec<Event>,
    }

    impl AddressBus for TraceBus {
        fn read_byte(&mut self, _address: u32) -> u8 {
            0
        }

        fn read_word(&mut self, address: u32) -> u16 {
            self.events.push(Event::ReadWord(address));
            0x4e71
        }

        fn read_long(&mut self, _address: u32) -> u32 {
            0
        }

        fn write_byte(&mut self, _address: u32, _value: u8) {}

        fn write_word(&mut self, _address: u32, _value: u16) {}

        fn write_long(&mut self, _address: u32, _value: u32) {}

        fn sync(&mut self, cpu_clocks: u32) {
            self.events.push(Event::Sync(cpu_clocks));
        }

        fn ipl_hold_sample(&mut self) {
            self.events.push(Event::IplHold);
        }
    }

    fn cpu_with_one_prefetch_word() -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.pc = 0x2000;
        cpu.prefetch_queue = [0x4e71, 0];
        cpu.prefetch_count = 1;
        cpu
    }

    #[test]
    fn m68000_btst_register_prefetches_before_internal_sync() {
        let mut cpu = cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x10;

        let cycles = cpu.exec_btst(&mut bus, 4, AddressingMode::DataDirect(0));

        assert_eq!(cycles, 6);
        assert_eq!(cpu.not_z_flag, 1);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(bus.events, vec![Event::ReadWord(0x2002), Event::Sync(2)]);
    }

    #[test]
    fn m68000_bclr_register_poll_point_precedes_internal_sync() {
        let mut cpu = cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 1 << 20;

        let cycles = cpu.exec_bclr(&mut bus, 20, AddressingMode::DataDirect(0));

        assert_eq!(cycles, 10);
        assert_eq!(cpu.dar[0], 0);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(
            bus.events,
            vec![Event::ReadWord(0x2002), Event::IplHold, Event::Sync(6)]
        );
    }
}
