//! CALLM and RTM instructions (68020 only).
//!
//! These instructions were introduced in the 68020 but removed in the 68030+.
//! They implement a modular calling convention with automatic module state management.

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

impl CpuCore {
    /// Execute CALLM (Call Module) instruction.
    ///
    /// CALLM saves the current module state, loads a new module descriptor,
    /// and transfers control to the module entry point.
    ///
    /// Encoding: 0000 0110 11 mmm rrr + extension word (argument count)
    pub fn exec_callm<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        // CALLM is 68020-only; on other CPUs it should trigger Line-F or Illegal
        if self.cpu_type != CpuType::M68020 {
            return self.take_exception(bus, 11); // Line-F
        }

        // Read argument count from extension word (bits 7-0)
        let ext = self.read_imm_16(bus);
        let _arg_count = ext & 0xFF;

        // Decode effective address for module descriptor
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;

        let Some(mode) = AddressingMode::decode(ea_mode, ea_reg) else {
            return self.take_exception(bus, 4); // Illegal instruction
        };

        // Get module descriptor address
        let desc_addr = self.get_ea_address(bus, mode, Size::Long);

        // Module Descriptor format (minimal implementation):
        // +0: OPT/TYPE (byte) - Options and module type
        // +1: Access Level (byte)
        // +4: Entry Point (long)
        // +8: Data Area Pointer (long)
        // +12: (optional) Stack Pointer

        let _opt_type = self.read_8(bus, desc_addr);
        let _access_level = self.read_8(bus, desc_addr.wrapping_add(1));
        let entry_point = self.read_32(bus, desc_addr.wrapping_add(4));
        let _data_area = self.read_32(bus, desc_addr.wrapping_add(8));

        // Push module frame onto stack (simplified):
        // The full CALLM frame is complex; we implement a minimal version
        // that saves: return PC, saved module state marker

        // Push current PC (return address)
        self.push_32(bus, self.pc);

        // Push a marker indicating CALLM frame (for RTM to recognize)
        // Using high nibble 0xC to mark CALLM frame
        self.push_32(bus, 0xC0000000 | desc_addr);

        // Transfer control to module entry point
        self.pc = entry_point;

        60 // Approximate cycle count
    }

    /// Execute RTM (Return from Module) instruction.
    ///
    /// RTM restores the module state saved by CALLM and returns to the caller.
    ///
    /// Encoding: 0000 0110 1100 xrrr
    /// Where x=0 for Dn, x=1 for An (register containing module data pointer)
    pub fn exec_rtm<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        // RTM is 68020-only
        if self.cpu_type != CpuType::M68020 {
            return self.take_exception(bus, 11); // Line-F
        }

        // Pop the frame marker (simplified)
        let frame_marker = self.pull_32(bus);

        // Verify this is a CALLM frame
        if (frame_marker & 0xF0000000) != 0xC0000000 {
            // Invalid frame - could trigger exception, but for now just continue
            // In a real implementation this would be more rigorous
        }

        // Pop return address
        let return_pc = self.pull_32(bus);
        self.pc = return_pc;

        // Register argument (bits 2-0) contains destination for module data pointer
        // We don't use it in this simplified implementation
        let _reg = (opcode & 7) as usize;
        let _is_areg = (opcode & 8) != 0;

        40 // Approximate cycle count
    }
}
