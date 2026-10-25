//! 68040 integer timing: a single-issue pipeline model.
//!
//! The 68040's integer unit is the 68060's ancestor: a six-stage pipeline
//! that retires most integer instructions in one effective clock, with no
//! superscalar dispatch and no branch cache. Until now the part shared the
//! legacy 68000-count scaling approximation with the 68030, which real
//! hardware measures as ~2-2.5x pessimistic on plain execution: Copperline's
//! `timing-test/accelprobe.asm` columns from a real A4000 with a 25 MHz
//! A3640 (two byte-identical serial captures) measure a taken-`dbra` loop
//! at 4.06 clocks/iteration where the scaled approximation bills ~8, a
//! `move.w`+`dbra` loop at 4.08 (the one-clock body hides entirely in the
//! branch resolution), and `mulu.w #imm`+`dbra` at 14.0 where the
//! approximation bills ~35.
//!
//! The model here reuses the 68060 opcode classifier (`info_060`): most
//! ALU/move instructions cost their one-clock pOEP occupancy,
//! data-dependent and unclassified costs derive from the corrected 68000
//! reference count as `(raw/4).max(1)` (the same rule of thumb the 060
//! fallback uses), an indexed EA adds a clock, and branches pay small
//! static costs -- the 040 has no branch cache, so there is nothing to
//! predict or fold. Memory latency stays billed by the host bus per
//! access, exactly as on the 020 and 060 paths.
//!
//! Branch calibration: `DBcc`-taken is 3 clocks, so the ubiquitous
//! one-clock-body loop lands on the measured 4 clocks/iteration; the empty
//! `dbra` loop (rare in real code) then reads 3 against the measured 4.06,
//! which is the deliberate compromise -- a model without an
//! overlap/absorption stage cannot make both shapes exact, and the body
//! loop is the one real code runs. `mulu.w` lands at 13+3 = 16 against the
//! measured 14. Exception entries keep the legacy scaled costs unchanged.
//!
//! Residuals -- simplifications, not uniformly conservative: there is no
//! execute-stage overlap (pessimistic: a one-clock body would partially
//! hide ahead of a longer instruction on silicon), while shifts and bit
//! operations take the 060 class costs, which can be OPTIMISTIC where real
//! 040 silicon runs those forms a clock or two slower; the write-back
//! stage and store buffer are not modelled (writes bill at bus rate).

use super::cpu::CpuCore;
use super::timing_060::{F_BRANCH, F_DBCC, F_EA_INDEXED, F_UNCLASSIFIED, F_VARIABLE, info_060};

/// Taken Bcc/BRA/BSR: static pipeline refill, no prediction hardware.
pub const CYC_040_BRANCH_TAKEN: i32 = 3;
/// Not-taken conditional branch.
pub const CYC_040_BRANCH_NOT_TAKEN: i32 = 2;
/// Taken (looping) DBcc: 3 clocks, so a one-clock body + branch lands on
/// the measured 4 clocks per iteration.
pub const CYC_040_DBCC_TAKEN: i32 = 3;
/// Expired (falling through) DBcc.
pub const CYC_040_DBCC_EXPIRED: i32 = 3;
/// Floor for JMP/JSR/RTS/RTE/RTR and other computed flow changes.
pub const CYC_040_FLOW_MIN: i32 = 4;
/// CINV/CPUSH: the cache-maintenance pipeline runs for around a dozen
/// clocks even on a clean line (the real A3640 measures ~12 clocks per
/// `CPUSHL DC,(An)`, accelprobe row 15); wider scopes keep at least that.
pub const CYC_040_CACHE_OP: i32 = 12;

/// The 040 rule of thumb for costs not in the class table: a 4-clock
/// 68000 register operation is 1 clock. Same shape as the 060 fallback;
/// never the 020+ scaler, whose `.max(2)` floor would destroy one-clock
/// costs.
#[inline]
fn fallback_cycles(raw: i32) -> i32 {
    (raw / 4).max(1)
}

impl CpuCore {
    /// 68040 cycle cost for the instruction that just retired normally.
    /// `raw` is the handler's 68000-reference count, used for
    /// data-dependent fallbacks; exception entries keep the legacy scaled
    /// costs so the exception timing calibration is unchanged by the
    /// pipeline model.
    pub(crate) fn cycles_040(&mut self, raw: i32) -> i32 {
        if self.instruction_exception_vector.is_some() {
            return self.scale_cycles_for_cpu_type(raw);
        }
        let info = info_060(self.ir as u16);
        let flowed = self.change_of_flow;

        if info.has(F_BRANCH) {
            // The DBcc handler does not raise change_of_flow; a loop is
            // visible as a PC that is not the fall-through (ppc + 4).
            let taken = if info.has(F_DBCC) {
                self.pc != self.ppc.wrapping_add(4)
            } else {
                flowed
            };
            return match (info.has(F_DBCC), taken) {
                (true, true) => CYC_040_DBCC_TAKEN,
                (true, false) => CYC_040_DBCC_EXPIRED,
                (false, true) => CYC_040_BRANCH_TAKEN,
                (false, false) => CYC_040_BRANCH_NOT_TAKEN,
            };
        }
        if flowed {
            // JMP/JSR/RTS/RTE/RTR and friends: pipeline refill floor.
            return fallback_cycles(raw).max(CYC_040_FLOW_MIN);
        }
        let op = self.ir as u16;
        // CINV/CPUSH cache maintenance (F4xx).
        if op & 0xFF00 == 0xF400 {
            return fallback_cycles(raw).max(CYC_040_CACHE_OP);
        }
        // Data-dependent iterative units the 060 class table prices at that
        // part's fixed pipeline cost: the 040 multiplier and divider are
        // iterative, so derive their cost from the 68000 reference count.
        let is_muldiv = matches!(op & 0xF1C0, 0xC0C0 | 0xC1C0 | 0x80C0 | 0x81C0)
            || matches!(op & 0xFFC0, 0x4C00 | 0x4C40);
        let mut cycles = if is_muldiv || info.has(F_VARIABLE) || info.has(F_UNCLASSIFIED) {
            fallback_cycles(raw)
        } else {
            info.cycles()
        };
        if info.has(F_EA_INDEXED) {
            cycles += 1;
        }
        cycles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::CpuType;

    fn cpu_040() -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68040);
        cpu
    }

    #[test]
    fn simple_alu_ops_cost_one_clock() {
        let mut cpu = cpu_040();
        cpu.ir = 0x7001; // MOVEQ #1,D0
        assert_eq!(cpu.cycles_040(4), 1);
        cpu.ir = 0x3002; // MOVE.W D2,D0
        assert_eq!(cpu.cycles_040(4), 1);
        cpu.ir = 0xD280; // ADD.L D0,D1
        assert_eq!(cpu.cycles_040(4), 1);
    }

    #[test]
    fn taken_dbcc_makes_the_measured_one_clock_body_loop_four_clocks() {
        // The real-A4000 anchor (accelprobe rows 6/7): move.w d2,d0 + dbra
        // = 4.08 clk/iter, so body (1) + taken DBcc must be 4.
        let mut cpu = cpu_040();
        cpu.ir = 0x51CE; // DBRA D6,<disp>
        cpu.ppc = 0x1000;
        cpu.pc = 0x0FFC; // looped: not the fall-through
        assert_eq!(cpu.cycles_040(12), CYC_040_DBCC_TAKEN);
        cpu.pc = cpu.ppc.wrapping_add(4); // expired: fell through
        assert_eq!(cpu.cycles_040(14), CYC_040_DBCC_EXPIRED);
    }

    #[test]
    fn data_dependent_costs_derive_from_the_68000_reference() {
        // MULU.W #$5555 (8 set bits): 68000 reference 38+2*8 = 54 -> 13,
        // near the measured 14-clk loop (13 + taken dbra 3 = 16 modelled).
        let mut cpu = cpu_040();
        cpu.ir = 0xC0FC; // MULU.W #imm,D0
        assert_eq!(cpu.cycles_040(54), 13);
    }

    #[test]
    fn cache_line_maintenance_costs_a_dozen_clocks() {
        let mut cpu = cpu_040();
        cpu.ir = 0xF468; // CPUSHL DC,(A0)
        assert_eq!(cpu.cycles_040(8), CYC_040_CACHE_OP);
    }

    #[test]
    fn flow_changes_pay_the_refill_floor() {
        let mut cpu = cpu_040();
        cpu.ir = 0x4E75; // RTS
        cpu.change_of_flow = true;
        assert_eq!(cpu.cycles_040(16), CYC_040_FLOW_MIN);
    }
}
