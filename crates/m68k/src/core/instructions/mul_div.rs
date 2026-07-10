//! Multiply and divide instructions.
//!
//! MULS, MULU, DIVS, DIVU

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::execute::RUN_MODE_BERR_AERR_RESET;
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

/// MULS multiplier-dependent cycle count: the number of `01`/`10` bit pairs in
/// the source word with a 0 appended to the low end (bit transitions in the
/// 17-bit sequence `[src15..src0, 0]`). MULS = 38 + 2*this + EA on the 68000.
#[inline]
fn muls_transitions(src: u16) -> u32 {
    let s = (src as u32) << 1; // append a trailing 0 bit
    ((s ^ (s >> 1)) & 0xFFFF).count_ones()
}

#[inline]
fn mulu_internal_clocks(src: u16) -> u32 {
    34 + 2 * src.count_ones()
}

#[inline]
fn muls_internal_clocks(src: u16) -> u32 {
    34 + 2 * muls_transitions(src)
}

/// MC68000 DIVU.W compute cycles (excluding the EA fetch). `divisor` must be
/// non-zero. Early overflow (high word of the dividend >= divisor) terminates
/// fast; otherwise the data-dependent restoring-division loop is simulated.
#[inline]
fn divu_cycles(dividend: u32, divisor: u16) -> i32 {
    let div = divisor as u32;
    // Early overflow: quotient cannot fit in 16 bits.
    if (dividend >> 16) >= div {
        return 10;
    }
    let hdivisor = div << 16;
    let mut mcycles: i32 = 38;
    let mut dividend = dividend;
    for _ in 0..15 {
        let temp = dividend;
        dividend <<= 1;
        if (temp as i32) < 0 {
            dividend = dividend.wrapping_sub(hdivisor);
        } else {
            mcycles += 2;
            if dividend >= hdivisor {
                dividend = dividend.wrapping_sub(hdivisor);
                mcycles -= 1;
            }
        }
    }
    mcycles * 2
}

/// MC68000 DIVS.W compute cycles (excluding the EA fetch). `divisor` must be
/// non-zero. Signed division: a small base plus a negative-dividend penalty,
/// fast early-overflow termination, otherwise a quotient-bit-dependent loop.
#[inline]
fn divs_cycles(dividend: i32, divisor: i16) -> i32 {
    let mut mcycles: i32 = 6;
    if dividend < 0 {
        mcycles += 1;
    }
    let adivisor = (divisor as i32).unsigned_abs();
    let adividend = (dividend as i64).unsigned_abs() as u32;
    // Early overflow: |quotient| cannot fit in 15 bits.
    if (adividend >> 16) >= adivisor {
        return (mcycles + 2) * 2;
    }
    mcycles += 55;
    // A non-negative divisor saves one cycle for a non-negative dividend
    // and costs one for a negative dividend (WinUAE getDivs68kCycles).
    if divisor >= 0 {
        if dividend >= 0 {
            mcycles -= 1;
        } else {
            mcycles += 1;
        }
    }
    // Each leading 0 in the absolute quotient costs one extra cycle.
    let aquotient = adividend / adivisor;
    let mut q = (aquotient & 0xFFFF) as u16;
    for _ in 0..15 {
        if (q as i16) >= 0 {
            mcycles += 1;
        }
        q <<= 1;
    }
    mcycles * 2
}

impl CpuCore {
    fn finish_m68000_mul<B: AddressBus>(&mut self, bus: &mut B, internal_clocks: u32) {
        // 68000 MULU/MULS perform the final prefetch before the multiplier's
        // internal clocks, then write Dn after that sync interval.
        self.top_up_prefetch(bus);
        self.internal_cycles(internal_clocks);
        self.flush_sync(bus);
    }

    /// Execute MULU (unsigned 16x16 -> 32 multiply).
    ///
    /// MULU <ea>, Dn
    pub fn exec_mulu<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        let src = self.read_ea(bus, mode, Size::Word) & 0xFFFF;
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let dst = self.d(dst_reg) & 0xFFFF;
        let result = src * dst;

        if self.cpu_type == CpuType::M68000 {
            self.finish_m68000_mul(bus, mulu_internal_clocks(src as u16));
        }
        self.set_d(dst_reg, result);

        // Set flags
        self.not_z_flag = result;
        self.n_flag = if result & 0x80000000 != 0 { 0x80 } else { 0 };
        self.v_flag = 0;
        self.c_flag = 0;

        // MC68000: MULU.W = 38 + 2 * (ones in the 16-bit source) + EA.
        // 020+ has a fixed-cost multiplier; the cycle-exact A1200/FS-UAE
        // reference measures MULU.W at ~27 cycles, so pre-scale to 42
        // (-> 27 after scale_cycles_for_cpu_type).
        if self.cpu_type == CpuType::M68000 {
            38 + 2 * (src & 0xFFFF).count_ones() as i32
                + self.ea_source_cycles(mode, Size::Word)
        } else {
            42
        }
    }

    /// Execute MULS (signed 16x16 -> 32 multiply).
    ///
    /// MULS <ea>, Dn
    pub fn exec_muls<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        let src = self.read_ea(bus, mode, Size::Word) as i16 as i32;
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let dst = self.d(dst_reg) as i16 as i32;
        let result = (src * dst) as u32;

        if self.cpu_type == CpuType::M68000 {
            self.finish_m68000_mul(bus, muls_internal_clocks(src as u16));
        }
        self.set_d(dst_reg, result);

        // Set flags
        self.not_z_flag = result;
        self.n_flag = if result & 0x80000000 != 0 { 0x80 } else { 0 };
        self.v_flag = 0;
        self.c_flag = 0;

        // MC68000: MULS.W = 38 + 2 * (bit transitions in source<<1) + EA.
        // 020+ fixed-cost multiplier (~27 cycles measured); pre-scale 42.
        if self.cpu_type == CpuType::M68000 {
            38 + 2 * muls_transitions(src as u16) as i32
                + self.ea_source_cycles(mode, Size::Word)
        } else {
            42
        }
    }

    /// Execute DIVU (unsigned 32÷16 -> 16Q + 16R).
    ///
    /// DIVU <ea>, Dn
    /// Result: Dn[31:16] = remainder, Dn[15:0] = quotient
    pub fn exec_divu<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        let src = self.read_ea(bus, mode, Size::Word) & 0xFFFF;
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let dst = self.d(dst_reg);

        let m68000 = self.cpu_type == CpuType::M68000;
        if src == 0 {
            // Division by zero: 8 internal clocks precede the exception's
            // first stack write (Moira: SYNC(8) then the zero-divide
            // exception).
            if m68000 {
                self.internal_cycles(8);
            }
            return self.exception_zero_divide(bus);
        }

        let div_clocks = if m68000 { divu_cycles(dst, src as u16) } else { 140 };
        let cycles = if m68000 {
            div_clocks + self.ea_source_cycles(mode, Size::Word)
        } else {
            140
        };
        // The final prefetch precedes the division algorithm's internal
        // clocks (Moira: prefetch<POLL> then SYNC): the np is the
        // instruction's last bus access and carries the IPL poll, so an
        // interrupt rising during the division is taken one instruction
        // later.
        if m68000 {
            self.top_up_prefetch(bus);
            if div_clocks > 4 {
                self.internal_cycles((div_clocks - 4) as u32);
                // Advance the beam for the division's internal clocks now, before
                // the instruction boundary (Moira's SYNC runs immediately after
                // prefetch<POLL>). Deferring them past the boundary -- to the next
                // instruction's first bus access -- mistimes CPU-vs-bitplane-DMA
                // contention during an active display, doubling the beam cost of a
                // DIV run mid-fetch (timing-test row 31 8722 -> 4820 cck vs vAmiga
                // 4790; TEK Rampage's Ellis scene depends on it).
                self.flush_sync(bus);
            }
        }

        let quotient = dst / src;
        let remainder = dst % src;

        // Check for overflow (quotient must fit in 16 bits)
        if quotient >= 0x10000 {
            self.v_flag = 0x80;
            if self.sst_m68000_compat {
                // SingleStepTests/MAME fixtures expect deterministic N/Z on overflow.
                self.n_flag = 0x80;
                self.not_z_flag = 1; // Z=0
                self.c_flag = 0;
            }
            return cycles;
        }

        self.set_d(dst_reg, (remainder << 16) | (quotient & 0xFFFF));

        self.not_z_flag = quotient;
        self.n_flag = if quotient & 0x8000 != 0 { 0x80 } else { 0 };
        self.v_flag = 0;
        self.c_flag = 0;

        cycles
    }

    /// Execute DIVS (signed 32÷16 -> 16Q + 16R).
    ///
    /// DIVS <ea>, Dn
    /// Result: Dn[31:16] = remainder, Dn[15:0] = quotient
    pub fn exec_divs<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        let src = self.read_ea(bus, mode, Size::Word) as i16 as i32;
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            // Address/bus error while reading the operand: exception has been taken.
            return 50;
        }
        let dst = self.d(dst_reg) as i32;

        let m68000 = self.cpu_type == CpuType::M68000;
        if src == 0 {
            // Division by zero: 8 internal clocks precede the exception's
            // first stack write (Moira: SYNC(8) then the zero-divide
            // exception).
            if m68000 {
                self.internal_cycles(8);
            }
            return self.exception_zero_divide(bus);
        }

        let div_clocks = if m68000 { divs_cycles(dst, src as i16) } else { 158 };
        let cycles = if m68000 {
            div_clocks + self.ea_source_cycles(mode, Size::Word)
        } else {
            158
        };
        // The final prefetch precedes the division algorithm's internal
        // clocks (Moira: prefetch<POLL> then SYNC): the np is the
        // instruction's last bus access and carries the IPL poll, so an
        // interrupt rising during the division is taken one instruction
        // later.
        if m68000 {
            self.top_up_prefetch(bus);
            if div_clocks > 4 {
                self.internal_cycles((div_clocks - 4) as u32);
                // Advance the beam for the division's internal clocks now, before
                // the instruction boundary (Moira's SYNC runs immediately after
                // prefetch<POLL>). Deferring them past the boundary -- to the next
                // instruction's first bus access -- mistimes CPU-vs-bitplane-DMA
                // contention during an active display, doubling the beam cost of a
                // DIV run mid-fetch (timing-test row 31 8722 -> 4820 cck vs vAmiga
                // 4790; TEK Rampage's Ellis scene depends on it).
                self.flush_sync(bus);
            }
        }

        // Special case: 0x80000000 / -1 = 0x80000000 (would overflow)
        // But Musashi returns quotient=0, remainder=0 for this
        if dst == i32::MIN && src == -1 {
            self.set_d(dst_reg, 0);
            self.not_z_flag = 0;
            self.n_flag = 0;
            self.v_flag = 0;
            self.c_flag = 0;
            return cycles;
        }

        let quotient = dst / src;
        let remainder = dst % src;

        // Check for overflow (quotient must fit in signed 16 bits: -32768 to 32767)
        if !(-32768..=32767).contains(&quotient) {
            self.v_flag = 0x80;
            if self.sst_m68000_compat {
                // SingleStepTests/MAME fixtures expect deterministic N/Z on overflow.
                self.n_flag = 0x80;
                self.not_z_flag = 1; // Z=0
                self.c_flag = 0;
            }
            return cycles;
        }

        let quotient_u16 = quotient as i16 as u16 as u32;
        let remainder_u16 = remainder as i16 as u16 as u32;
        self.set_d(dst_reg, (remainder_u16 << 16) | quotient_u16);

        self.not_z_flag = quotient_u16;
        self.n_flag = if quotient_u16 & 0x8000 != 0 { 0x80 } else { 0 };
        self.v_flag = 0;
        self.c_flag = 0;

        cycles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        ReadWord(u32),
        Sync(u32),
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
    }

    fn m68000_cpu_with_one_prefetch_word() -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.pc = 0x2000;
        cpu.prefetch_queue = [0x4e71, 0];
        cpu.prefetch_count = 1;
        cpu
    }

    #[test]
    fn m68000_mulu_data_register_prefetches_before_multiplier_sync() {
        let mut cpu = m68000_cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x0000_0003;
        cpu.dar[1] = 0x0000_0004;

        let cycles = cpu.exec_mulu(&mut bus, AddressingMode::DataDirect(0), 1);

        assert_eq!(cycles, 42);
        assert_eq!(cpu.dar[1], 0x0000_000C);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(bus.events, vec![Event::ReadWord(0x2002), Event::Sync(38)]);
    }

    #[test]
    fn m68000_muls_data_register_prefetches_before_multiplier_sync() {
        let mut cpu = m68000_cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x0000_FFFF;
        cpu.dar[1] = 0x0000_0002;

        let cycles = cpu.exec_muls(&mut bus, AddressingMode::DataDirect(0), 1);

        assert_eq!(cycles, 40);
        assert_eq!(cpu.dar[1], 0xFFFF_FFFE);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(bus.events, vec![Event::ReadWord(0x2002), Event::Sync(36)]);
    }
}
