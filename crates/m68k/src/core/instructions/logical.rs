//! Logical instructions.
//!
//! AND, OR, EOR, NOT (NOT is in integer_arith.rs)

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::execute::RUN_MODE_BERR_AERR_RESET;
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

impl CpuCore {
    fn finish_immediate_sr_write<B: AddressBus>(&mut self, bus: &mut B, sr: u16) {
        if self.cpu_type == CpuType::M68000 {
            // On the 68000 the SR mutation happens after the status-op
            // internal delay, before the post-write refill.
            self.internal_cycles(8);
            self.flush_sync(bus);
            self.trace_t0_sr_write();
            self.set_sr(sr);
        } else {
            self.trace_t0_sr_write();
            self.set_sr(sr);
            self.internal_cycles(8);
        }
        self.full_prefetch(bus);
    }

    pub(crate) fn finish_m68000_immediate_data_register_write<B: AddressBus>(
        &mut self,
        bus: &mut B,
        reg: usize,
        size: Size,
        value: u32,
    ) {
        if self.cpu_type == CpuType::M68000 {
            self.top_up_prefetch(bus);
            self.ipl_poll_point(bus);
            if self.run_mode == RUN_MODE_BERR_AERR_RESET {
                return;
            }
            if size == Size::Long {
                self.internal_cycles(4);
                self.flush_sync(bus);
            }
        }

        match size {
            Size::Byte => self.dar[reg] = (self.dar[reg] & 0xFFFF_FF00) | (value & 0xFF),
            Size::Word => self.dar[reg] = (self.dar[reg] & 0xFFFF_0000) | (value & 0xFFFF),
            Size::Long => self.dar[reg] = value,
        }
    }

    /// Execute AND instruction.
    pub fn exec_and<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst: u32,
    ) -> (u32, i32) {
        let result = src & dst & size.mask();
        self.set_logic_flags(result, size);
        (result, 4)
    }

    /// Execute ANDI instruction (immediate).
    pub fn exec_andi<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let imm = match size {
            Size::Byte => self.read_imm_16(bus) as u32 & 0xFF,
            Size::Word => self.read_imm_16(bus) as u32,
            Size::Long => self.read_imm_32(bus),
        };
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        if self.cpu_type == CpuType::M68000
            && let AddressingMode::DataDirect(reg) = mode
        {
            let dst = self.d(reg as usize) & size.mask();
            let (result, _) = self.exec_and::<B>(bus, size, imm, dst);
            self.finish_m68000_immediate_data_register_write(bus, reg as usize, size, result);
            return if size == Size::Long { 16 } else { 8 };
        }
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        let (result, _) = self.exec_and::<B>(bus, size, imm, dst);
        // ANDI/ORI/EORI to memory poll IPL during the pre-writeback prefetch.
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        if size == Size::Long { 16 } else { 8 }
    }

    /// Execute ANDI to CCR.
    pub fn exec_andi_ccr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        let imm = self.read_imm_16(bus) as u8;
        let ccr = self.get_ccr() & imm;
        self.set_ccr(ccr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
        20
    }

    /// Execute ANDI to SR.
    pub fn exec_andi_sr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        if !self.is_supervisor() {
            return self.exception_privilege(bus);
        }
        let imm = self.read_imm_16(bus);
        let sr = self.get_sr() & imm;
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.finish_immediate_sr_write(bus, sr);
        20
    }

    /// Execute OR instruction.
    pub fn exec_or<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst: u32,
    ) -> (u32, i32) {
        let result = (src | dst) & size.mask();
        self.set_logic_flags(result, size);
        (result, 4)
    }

    /// Execute ORI instruction (immediate).
    pub fn exec_ori<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let imm = match size {
            Size::Byte => self.read_imm_16(bus) as u32 & 0xFF,
            Size::Word => self.read_imm_16(bus) as u32,
            Size::Long => self.read_imm_32(bus),
        };
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        if self.cpu_type == CpuType::M68000
            && let AddressingMode::DataDirect(reg) = mode
        {
            let dst = self.d(reg as usize) & size.mask();
            let (result, _) = self.exec_or::<B>(bus, size, imm, dst);
            self.finish_m68000_immediate_data_register_write(bus, reg as usize, size, result);
            return if size == Size::Long { 16 } else { 8 };
        }
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let result = (imm | dst) & size.mask();
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while writing the operand: exception has been taken.
            return 50;
        }
        self.set_logic_flags(result, size);
        if size == Size::Long { 16 } else { 8 }
    }

    /// Execute ORI to CCR.
    pub fn exec_ori_ccr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        let imm = self.read_imm_16(bus) as u8;
        let ccr = self.get_ccr() | imm;
        self.set_ccr(ccr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
        20
    }

    /// Execute ORI to SR.
    pub fn exec_ori_sr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        if !self.is_supervisor() {
            return self.exception_privilege(bus);
        }
        let imm = self.read_imm_16(bus);
        let sr = self.get_sr() | imm;
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.finish_immediate_sr_write(bus, sr);
        20
    }

    /// Execute EOR instruction.
    pub fn exec_eor<B: AddressBus>(
        &mut self,
        _bus: &mut B,
        size: Size,
        src: u32,
        dst: u32,
    ) -> (u32, i32) {
        let result = (src ^ dst) & size.mask();
        self.set_logic_flags(result, size);
        (result, 4)
    }

    /// Execute EORI instruction (immediate).
    pub fn exec_eori<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
    ) -> i32 {
        let imm = match size {
            Size::Byte => self.read_imm_16(bus) as u32 & 0xFF,
            Size::Word => self.read_imm_16(bus) as u32,
            Size::Long => self.read_imm_32(bus),
        };
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        if self.cpu_type == CpuType::M68000
            && let AddressingMode::DataDirect(reg) = mode
        {
            let dst = self.d(reg as usize) & size.mask();
            let (result, _) = self.exec_eor::<B>(bus, size, imm, dst);
            self.finish_m68000_immediate_data_register_write(bus, reg as usize, size, result);
            return if size == Size::Long { 16 } else { 8 };
        }
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        let result = (imm ^ dst) & size.mask();
        self.write_resolved_ea_np_poll(bus, ea, size, result);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        self.set_logic_flags(result, size);
        if size == Size::Long { 16 } else { 8 }
    }

    /// Execute EORI to CCR.
    pub fn exec_eori_ccr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        let imm = self.read_imm_16(bus) as u8;
        let ccr = self.get_ccr() ^ imm;
        self.set_ccr(ccr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
        20
    }

    /// Execute EORI to SR.
    pub fn exec_eori_sr<B: AddressBus>(&mut self, bus: &mut B) -> i32 {
        if !self.is_supervisor() {
            return self.exception_privilege(bus);
        }
        let imm = self.read_imm_16(bus);
        let sr = self.get_sr() ^ imm;
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.finish_immediate_sr_write(bus, sr);
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            if address == 0x2002 { 0x0000 } else { 0x4e71 }
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

    fn m68000_cpu_with_one_immediate_word() -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.pc = 0x2000;
        cpu.prefetch_queue = [0xffff, 0];
        cpu.prefetch_count = 1;
        cpu
    }

    #[test]
    fn m68000_andi_long_data_register_prefetches_before_write() {
        let mut cpu = m68000_cpu_with_one_immediate_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x1234_5678;

        let cycles = cpu.exec_andi(&mut bus, Size::Long, AddressingMode::DataDirect(0));

        assert_eq!(cycles, 16);
        assert_eq!(cpu.d(0), 0x1234_0000);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(
            bus.events,
            vec![
                Event::ReadWord(0x2002),
                Event::ReadWord(0x2004),
                Event::ReadWord(0x2006),
                Event::IplHold,
                Event::Sync(4)
            ]
        );
    }

    #[test]
    fn m68000_andi_sr_syncs_before_refill() {
        let mut cpu = m68000_cpu_with_one_immediate_word();
        let mut bus = TraceBus::default();
        cpu.set_sr(0x270f);

        let cycles = cpu.exec_andi_sr(&mut bus);

        assert_eq!(cycles, 20);
        assert_eq!(cpu.get_sr(), 0x270f);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(
            bus.events,
            vec![
                Event::ReadWord(0x2002),
                Event::Sync(8),
                Event::ReadWord(0x2002),
                Event::ReadWord(0x2004)
            ]
        );
    }
}
