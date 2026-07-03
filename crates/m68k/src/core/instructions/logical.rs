//! Logical instructions.
//!
//! AND, OR, EOR, NOT (NOT is in integer_arith.rs)

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::execute::RUN_MODE_BERR_AERR_RESET;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

impl CpuCore {
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
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        let (result, _) = self.exec_and::<B>(bus, size, imm, dst);
        self.write_resolved_ea(bus, ea, size, result);
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
        self.trace_t0_sr_write();
        self.set_sr(sr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
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
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let result = (imm | dst) & size.mask();
        self.write_resolved_ea(bus, ea, size, result);
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
        self.trace_t0_sr_write();
        self.set_sr(sr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
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
        let ea = self.resolve_ea(bus, mode, size);
        let dst = self.read_resolved_ea(bus, ea, size);
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return 50;
        }
        let result = (imm ^ dst) & size.mask();
        self.write_resolved_ea(bus, ea, size, result);
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
        self.trace_t0_sr_write();
        self.set_sr(sr);
        // Status modification spends 8 internal clocks before discarding and
        // refilling the prefetch queue.
        self.internal_cycles(8);
        self.full_prefetch(bus);
        20
    }
}
