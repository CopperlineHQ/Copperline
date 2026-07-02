//! Effective Address resolution.
//!
//! Implements all M68000 addressing modes.

use super::cpu::CpuCore;
use super::execute::RUN_MODE_BERR_AERR_RESET;
use super::memory::AddressBus;
use super::types::{CpuType, Size};

/// Addressing mode encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    /// Data Register Direct: Dn
    DataDirect(u8),
    /// Address Register Direct: An
    AddressDirect(u8),
    /// Address Register Indirect: (An)
    AddressIndirect(u8),
    /// Address Register Indirect with Post-Increment: (An)+
    PostIncrement(u8),
    /// Address Register Indirect with Pre-Decrement: -(An)
    PreDecrement(u8),
    /// Address Register Indirect with Displacement: (d16,An)
    Displacement(u8),
    /// Address Register Indirect with Index: (d8,An,Xn)
    Index(u8),
    /// Absolute Short: (xxx).W
    AbsoluteShort,
    /// Absolute Long: (xxx).L  
    AbsoluteLong,
    /// PC with Displacement: (d16,PC)
    PcDisplacement,
    /// PC with Index: (d8,PC,Xn)
    PcIndex,
    /// Immediate: #<data>
    Immediate,
}

impl AddressingMode {
    /// Decode mode and register fields from opcode.
    #[inline]
    pub fn decode(mode: u8, reg: u8) -> Option<Self> {
        match mode {
            0b000 => Some(Self::DataDirect(reg)),
            0b001 => Some(Self::AddressDirect(reg)),
            0b010 => Some(Self::AddressIndirect(reg)),
            0b011 => Some(Self::PostIncrement(reg)),
            0b100 => Some(Self::PreDecrement(reg)),
            0b101 => Some(Self::Displacement(reg)),
            0b110 => Some(Self::Index(reg)),
            0b111 => match reg {
                0b000 => Some(Self::AbsoluteShort),
                0b001 => Some(Self::AbsoluteLong),
                0b010 => Some(Self::PcDisplacement),
                0b011 => Some(Self::PcIndex),
                0b100 => Some(Self::Immediate),
                _ => None,
            },
            _ => None,
        }
    }

    /// Check if this mode is a register direct mode.
    #[inline]
    pub fn is_register_direct(&self) -> bool {
        matches!(self, Self::DataDirect(_) | Self::AddressDirect(_))
    }
}

/// Result of effective address calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EaResult {
    /// Value is in a data register.
    DataReg(u8),
    /// Value is in an address register.
    AddrReg(u8),
    /// Value is at a memory address.
    Memory(u32),
    /// Immediate operand, already consumed from the instruction stream.
    ///
    /// The payload is the operand VALUE (not an address): immediate data is
    /// part of the instruction stream and on a prefetch-modeled 68000 it has
    /// already passed through the prefetch queue by the time the EA is
    /// resolved, so it cannot be re-read from memory as data.
    Immediate(u32),
}

impl CpuCore {
    /// Get increment/decrement size for address register.
    /// Stack pointer (A7) always uses word alignment minimum.
    #[inline]
    fn addr_inc(&self, reg: u8, size: Size) -> u32 {
        if reg == 7 && size == Size::Byte {
            2 // A7 always word-aligned
        } else {
            size.bytes()
        }
    }

    /// Resolve effective address.
    pub fn resolve_ea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        size: Size,
    ) -> EaResult {
        // If we're in the middle of processing a bus/address error, avoid applying additional
        // EA side effects (postinc/predec) from the faulting instruction.
        if self.run_mode == RUN_MODE_BERR_AERR_RESET {
            return EaResult::Memory(0);
        }
        match mode {
            AddressingMode::DataDirect(reg) => EaResult::DataReg(reg),
            AddressingMode::AddressDirect(reg) => EaResult::AddrReg(reg),
            AddressingMode::AddressIndirect(reg) => EaResult::Memory(self.a(reg as usize)),
            AddressingMode::PostIncrement(reg) => {
                let addr = self.a(reg as usize);
                let inc = self.addr_inc(reg, size);
                self.set_a(reg as usize, addr.wrapping_add(inc));
                EaResult::Memory(addr)
            }
            AddressingMode::PreDecrement(reg) => {
                // The predecrement address computation costs 2 internal
                // clocks before the operand access.
                self.internal_cycles(2);
                let dec = self.addr_inc(reg, size);
                let addr = self.a(reg as usize).wrapping_sub(dec);
                self.set_a(reg as usize, addr);
                EaResult::Memory(addr)
            }
            AddressingMode::Displacement(reg) => {
                let disp = self.read_imm_16(bus) as i16 as i32;
                let addr = (self.a(reg as usize) as i32).wrapping_add(disp) as u32;
                EaResult::Memory(addr)
            }
            AddressingMode::Index(reg) => {
                // Brief-extension-word index computation costs 2 internal
                // clocks BEFORE the extension fetch.
                self.internal_cycles(2);
                let ext = self.read_imm_16(bus);
                let addr = self.compute_index(self.a(reg as usize), ext, bus);
                EaResult::Memory(addr)
            }
            AddressingMode::AbsoluteShort => {
                let addr = self.read_imm_16(bus) as i16 as i32 as u32;
                EaResult::Memory(addr)
            }
            AddressingMode::AbsoluteLong => {
                let addr = self.read_imm_32(bus);
                EaResult::Memory(addr)
            }
            AddressingMode::PcDisplacement => {
                let pc = self.pc;
                let disp = self.read_imm_16(bus) as i16 as i32;
                let addr = (pc as i32).wrapping_add(disp) as u32;
                EaResult::Memory(addr)
            }
            AddressingMode::PcIndex => {
                // Brief-extension-word index computation costs 2 internal
                // clocks BEFORE the extension fetch.
                self.internal_cycles(2);
                let pc = self.pc;
                let ext = self.read_imm_16(bus);
                let addr = self.compute_index(pc, ext, bus);
                EaResult::Memory(addr)
            }
            AddressingMode::Immediate => {
                // Consume the immediate data from the instruction stream now.
                // It must go through read_imm_* (the prefetch queue on 68000)
                // rather than being re-read later as a data access.
                let value = match size {
                    Size::Byte => (self.read_imm_16(bus) & 0xFF) as u32,
                    Size::Word => self.read_imm_16(bus) as u32,
                    Size::Long => self.read_imm_32(bus),
                };
                EaResult::Immediate(value)
            }
        }
    }

    /// Compute indexed address from extension word.
    fn compute_index<B: AddressBus>(&mut self, base: u32, ext: u16, bus: &mut B) -> u32 {
        let d8 = (ext & 0xFF) as i8 as i32;
        let idx_reg = ((ext >> 12) & 0xF) as usize;
        let idx_is_addr = (ext & 0x8000) != 0;
        let idx_is_long = (ext & 0x0800) != 0;
        // Scale is a 68020+ feature (brief extension word on 68000/68010 does not scale).
        // Some test generators may leave non-zero scale bits in the extension word; 68000 ignores them.
        let scale = if self.is_020_plus() {
            let scale_shift = ((ext >> 9) & 0x3) as u32; // 00=1,01=2,10=4,11=8
            1i32 << scale_shift
        } else {
            1i32
        };

        let idx_val = if idx_is_addr {
            self.dar[8 + (idx_reg & 7)]
        } else {
            self.dar[idx_reg & 7]
        };

        let idx_val = if idx_is_long {
            idx_val as i32
        } else {
            (idx_val as i16) as i32
        };
        let idx_val = idx_val.wrapping_mul(scale);

        // 68020+ full extension word format
        if (ext & 0x0100) != 0 && self.is_020_plus() {
            self.compute_full_index(base, ext, idx_val, bus)
        } else {
            // Brief format
            (base as i32).wrapping_add(d8).wrapping_add(idx_val) as u32
        }
    }

    /// 68020+ full extension word format.
    fn compute_full_index<B: AddressBus>(
        &mut self,
        base: u32,
        ext: u16,
        idx_val: i32,
        bus: &mut B,
    ) -> u32 {
        let bs = (ext & 0x0080) != 0; // Base suppress
        let is = (ext & 0x0040) != 0; // Index suppress
        let bd_size = (ext >> 4) & 0x03;
        let i_is = ext & 0x07;

        let base = if bs { 0 } else { base };
        let idx = if is { 0 } else { idx_val };

        let bd: i32 = match bd_size {
            0 | 1 => 0, // Reserved / Null
            2 => self.read_imm_16(bus) as i16 as i32,
            3 => self.read_imm_32(bus) as i32,
            _ => 0,
        };

        // Memory indirect modes
        if i_is != 0 {
            let outer_disp = match i_is & 0x03 {
                0 | 1 => 0i32,
                2 => self.read_imm_16(bus) as i16 as i32,
                3 => self.read_imm_32(bus) as i32,
                _ => 0,
            };

            if (i_is & 0x04) != 0 {
                // Post-indexed
                let intermediate = (base as i32).wrapping_add(bd) as u32;
                let indirect = self.read_32(bus, intermediate) as i32;
                indirect.wrapping_add(idx).wrapping_add(outer_disp) as u32
            } else {
                // Pre-indexed
                let intermediate = (base as i32).wrapping_add(bd).wrapping_add(idx) as u32;
                let indirect = self.read_32(bus, intermediate) as i32;
                indirect.wrapping_add(outer_disp) as u32
            }
        } else {
            // No memory indirect
            (base as i32).wrapping_add(bd).wrapping_add(idx) as u32
        }
    }

    /// Check if CPU is 68020 or later.
    #[inline]
    fn is_020_plus(&self) -> bool {
        matches!(
            self.cpu_type,
            CpuType::M68EC020
                | CpuType::M68020
                | CpuType::M68EC030
                | CpuType::M68030
                | CpuType::M68EC040
                | CpuType::M68LC040
                | CpuType::M68040
                | CpuType::M68060
        )
    }

    /// Read immediate 16-bit value and advance PC.
    ///
    /// 68000 (prefetch-modeled): the word comes from the prefetch queue and
    /// the queue refills with an overlap fetch of the word at `pc + 4` -- the
    /// bus access real hardware performs while this word is consumed. Other
    /// CPU types read directly at PC.
    #[inline]
    pub fn read_imm_16<B: AddressBus>(&mut self, bus: &mut B) -> u16 {
        let addr = self.pc;
        if (addr & 1) != 0 {
            self.trigger_address_error(bus, addr, false, true);
            return 0;
        }
        if self.prefetch_enabled() {
            return self.read_imm_16_prefetched(bus);
        }
        let mut addr = self.address(addr);
        if self.has_pmmu && self.pmmu_enabled {
            match crate::mmu::translate_address(
                self,
                bus,
                addr,
                /*write=*/ false,
                self.is_supervisor(),
                /*instruction=*/ true,
            ) {
                Ok(p) => addr = self.address(p),
                Err(f) => {
                    self.handle_mmu_fault(
                        bus, f, /*write=*/ false, /*instruction=*/ true,
                    );
                    return 0;
                }
            }
        }
        match bus.try_read_immediate_word(addr) {
            Ok(v) => {
                self.pc = self.pc.wrapping_add(2);
                v
            }
            Err(_) => {
                self.trigger_bus_error(bus, addr, false, true);
                0
            }
        }
    }

    /// Prefetch-queue path of `read_imm_16` (68000 only).
    ///
    /// Consuming an extension/immediate word takes it from the queue and
    /// immediately performs the accompanying "np" prefetch (one fetch of the
    /// next instruction-stream word the queue lacks). This is the order real
    /// hardware uses: extension-word prefetches happen during EA calculation,
    /// BEFORE the instruction's data accesses; only the final prefetch comes
    /// after the writes (the end-of-instruction `top_up_prefetch`).
    fn read_imm_16_prefetched<B: AddressBus>(&mut self, bus: &mut B) -> u16 {
        let word = self.consume_imm_16_no_prefetch(bus);
        if self.run_mode == super::execute::RUN_MODE_BERR_AERR_RESET {
            return word;
        }
        // The np that accompanies an extension-word consume -- suppressed in
        // flow-change microcode mode (the queue is about to be discarded).
        if !self.consume_without_prefetch {
            self.top_up_prefetch_one(bus);
        }
        word
    }

    /// Read immediate 32-bit value and advance PC.
    #[inline]
    pub fn read_imm_32<B: AddressBus>(&mut self, bus: &mut B) -> u32 {
        let addr = self.pc;
        if (addr & 1) != 0 {
            self.trigger_address_error(bus, addr, false, true);
            return 0;
        }
        if self.prefetch_enabled() {
            // Two queue consumptions: high word first, each with its overlap
            // fetch, exactly as the 68000 streams a 32-bit immediate.
            let hi = self.read_imm_16_prefetched(bus);
            if self.run_mode == super::execute::RUN_MODE_BERR_AERR_RESET {
                return 0;
            }
            let lo = self.read_imm_16_prefetched(bus);
            return ((hi as u32) << 16) | lo as u32;
        }
        let mut addr = self.address(addr);
        if self.has_pmmu && self.pmmu_enabled {
            match crate::mmu::translate_address(
                self,
                bus,
                addr,
                /*write=*/ false,
                self.is_supervisor(),
                /*instruction=*/ true,
            ) {
                Ok(p) => addr = self.address(p),
                Err(f) => {
                    self.handle_mmu_fault(
                        bus, f, /*write=*/ false, /*instruction=*/ true,
                    );
                    return 0;
                }
            }
        }
        match bus.try_read_immediate_long(addr) {
            Ok(v) => {
                self.pc = self.pc.wrapping_add(4);
                v
            }
            Err(_) => {
                self.trigger_bus_error(bus, addr, false, true);
                0
            }
        }
    }

    /// Read immediate 8-bit value and advance PC (reads word, returns low byte).
    #[inline]
    pub fn read_imm_8<B: AddressBus>(&mut self, bus: &mut B) -> u8 {
        (self.read_imm_16(bus) & 0xFF) as u8
    }

    /// Fetch the instruction opcode word at PC.
    ///
    /// On the prefetch-modeled 68000 the opcode comes from the queue without
    /// a bus access (the previous instruction's final prefetch loaded it).
    /// Other CPU types fetch directly at PC.
    pub fn fetch_opcode<B: AddressBus>(&mut self, bus: &mut B) -> u16 {
        if self.prefetch_enabled() {
            let addr = self.pc;
            if (addr & 1) != 0 {
                self.trigger_address_error(bus, addr, false, true);
                return 0;
            }
            return self.consume_imm_16_no_prefetch(bus);
        }
        self.read_imm_16(bus)
    }
}

/// MC68000 effective-address timing helpers (Motorola M68000 User's Manual
/// "Effective Address Calculation Times"). These return the cycles to compute
/// the address *and* perform the single operand memory access, for byte/word
/// (long in parentheses). Register/immediate forms include their fetch cost.
///
/// `source` is the read cost (used as the `<ea>` operand of an instruction);
/// `dest` is the write/address cost used for the destination `<ea>` of a MOVE
/// (no read). They are equal for the indirect modes (one memory cycle either
/// way) but the index/displacement extension-word costs are identical too --
/// the difference is only that `dest` omits nothing here because the access is
/// a write rather than a read of the same width.
impl CpuCore {
    /// Source-operand EA fetch time (address calc + one read), byte/word(long).
    #[inline]
    pub(crate) fn ea_source_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        use AddressingMode::*;
        let long = size == Size::Long;
        match mode {
            DataDirect(_) | AddressDirect(_) => 0,
            AddressIndirect(_) | PostIncrement(_) => {
                if long {
                    8
                } else {
                    4
                }
            }
            PreDecrement(_) => {
                if long {
                    10
                } else {
                    6
                }
            }
            Displacement(_) | AbsoluteShort | PcDisplacement => {
                if long {
                    12
                } else {
                    8
                }
            }
            Index(_) | PcIndex => {
                if long {
                    14
                } else {
                    10
                }
            }
            AbsoluteLong => {
                if long {
                    16
                } else {
                    12
                }
            }
            Immediate => {
                if long {
                    8
                } else {
                    4
                }
            }
        }
    }

    /// Destination-operand EA time for a MOVE (address calc + one write),
    /// byte/word(long). The MOVE destination predecrement costs the same as the
    /// other indirect modes (unlike a source predecrement, which adds 2).
    #[inline]
    pub(crate) fn ea_dest_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        use AddressingMode::*;
        let long = size == Size::Long;
        match mode {
            DataDirect(_) | AddressDirect(_) => 0,
            AddressIndirect(_) | PostIncrement(_) | PreDecrement(_) => {
                if long {
                    8
                } else {
                    4
                }
            }
            Displacement(_) | AbsoluteShort | PcDisplacement => {
                if long {
                    12
                } else {
                    8
                }
            }
            Index(_) | PcIndex => {
                if long {
                    14
                } else {
                    10
                }
            }
            AbsoluteLong => {
                if long {
                    16
                } else {
                    12
                }
            }
            Immediate => 0,
        }
    }

    /// True for addressing modes that fetch the operand from memory (used by the
    /// long-operand ALU rule: the 6-cycle long base rises to 8 when the source
    /// is a data/address register or immediate, since there is no memory fetch
    /// to overlap the long ALU operation).
    #[inline]
    pub(crate) fn ea_is_memory(mode: AddressingMode) -> bool {
        !matches!(
            mode,
            AddressingMode::DataDirect(_)
                | AddressingMode::AddressDirect(_)
                | AddressingMode::Immediate
        )
    }

    /// MC68000 timing for an `<ea>,Dn` ALU op (ADD/SUB/AND/OR, and the CMP/EOR
    /// read variants): base 4 byte/word; long base 6 for a memory source, 8 for
    /// register/immediate -- plus the source-fetch EA time.
    #[inline]
    pub(crate) fn alu_ea_dn_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        let base = if size == Size::Long {
            if Self::ea_is_memory(mode) { 6 } else { 8 }
        } else {
            4
        };
        base + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for ADDA/SUBA `<ea>,An`: word base 8; long base 6 memory /
    /// 8 register-immediate, plus source-fetch EA.
    #[inline]
    pub(crate) fn adda_suba_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        let base = if size == Size::Long {
            if Self::ea_is_memory(mode) { 6 } else { 8 }
        } else {
            8
        };
        base + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for CMPA `<ea>,An`: base 6 for word and long, plus
    /// source-fetch EA.
    #[inline]
    pub(crate) fn cmpa_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        6 + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for a `Dn,<ea>` ALU op writing back to a memory
    /// destination (ADD/SUB/AND/OR/EOR with memory `<ea>`): the destination is
    /// read (source-fetch EA cost) then the operation + writeback adds base 8
    /// byte/word, 12 long.
    #[inline]
    pub(crate) fn alu_dn_ea_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        let base = if size == Size::Long { 12 } else { 8 };
        base + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for an immediate ALU op (ADDI/SUBI/ANDI/ORI/EORI) with a
    /// data-register destination (8 byte/word, 16 long) or a memory destination
    /// (12 byte/word, 20 long, plus the destination read/EA cost).
    #[inline]
    pub(crate) fn immediate_alu_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        if Self::ea_is_memory(mode) {
            let base = if size == Size::Long { 20 } else { 12 };
            base + self.ea_source_cycles(mode, size)
        } else if size == Size::Long {
            16
        } else {
            8
        }
    }

    /// MOVEM address-calculation time (added to the 8/12 base + per-register
    /// cost). Predecrement/postincrement/(An) are free; displacement/absolute-
    /// short/PC-relative cost 4, indexed 6, absolute-long 8.
    #[inline]
    pub(crate) fn movem_ea_calc_cycles(&self, mode: AddressingMode) -> i32 {
        use AddressingMode::*;
        match mode {
            AddressIndirect(_) | PostIncrement(_) | PreDecrement(_) => 0,
            Displacement(_) | AbsoluteShort | PcDisplacement => 4,
            Index(_) | PcIndex => 6,
            AbsoluteLong => 8,
            _ => 0,
        }
    }

    /// Control-mode address-calculation time for LEA/PEA (no operand fetch).
    /// LEA = 4 + this; PEA = 12 + this.
    #[inline]
    pub(crate) fn control_addr_calc_cycles(&self, mode: AddressingMode) -> i32 {
        use AddressingMode::*;
        match mode {
            AddressIndirect(_) => 0,
            Displacement(_) | AbsoluteShort | PcDisplacement => 4,
            Index(_) | PcIndex | AbsoluteLong => 8,
            _ => 0,
        }
    }

    /// Control-mode address time for JMP/JSR. JMP = 8 + this; JSR = 16 + this.
    #[inline]
    pub(crate) fn jump_addr_calc_cycles(&self, mode: AddressingMode) -> i32 {
        use AddressingMode::*;
        match mode {
            AddressIndirect(_) => 0,
            Displacement(_) | AbsoluteShort | PcDisplacement => 2,
            AbsoluteLong => 4,
            Index(_) | PcIndex => 6,
            _ => 0,
        }
    }

    /// MC68000 timing for a bit op (BTST/BCHG/BCLR/BSET). `bit_op`: 0=BTST,
    /// 1=BCHG, 2=BCLR, 3=BSET. Register (Dn) operands act on the long: BTST 6,
    /// BCHG/BSET 6, BCLR 8. Memory operands act on a byte: BTST 4, others 8,
    /// plus the source EA. The static (immediate bit-number) form adds 4 for the
    /// extension-word fetch.
    #[inline]
    pub(crate) fn bitop_cycles(&self, mode: AddressingMode, bit_op: u16, is_static: bool) -> i32 {
        let static_add = if is_static { 4 } else { 0 };
        if matches!(mode, AddressingMode::DataDirect(_)) {
            let base = if bit_op == 2 { 8 } else { 6 };
            base + static_add
        } else {
            let base = if bit_op == 0 { 4 } else { 8 };
            base + static_add + self.ea_source_cycles(mode, Size::Byte)
        }
    }

    /// MC68000 timing for a unary read-modify-write `<ea>` op (CLR/NEG/NEGX/
    /// NOT): data register 4 byte/word, 6 long; memory 8 byte/word, 12 long plus
    /// the destination read/EA cost.
    #[inline]
    pub(crate) fn unary_rmw_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        if Self::ea_is_memory(mode) {
            let base = if size == Size::Long { 12 } else { 8 };
            base + self.ea_source_cycles(mode, size)
        } else if size == Size::Long {
            6
        } else {
            4
        }
    }

    /// MC68000 timing for `TST <ea>` (read only): base 4 plus the source EA.
    #[inline]
    pub(crate) fn tst_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        4 + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for CMPI `#imm,<ea>`: data register 8 byte/word, 14 long;
    /// memory 8 byte/word, 12 long, plus the destination read/EA cost.
    #[inline]
    pub(crate) fn cmpi_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        if Self::ea_is_memory(mode) {
            let base = if size == Size::Long { 12 } else { 8 };
            base + self.ea_source_cycles(mode, size)
        } else if size == Size::Long {
            14
        } else {
            8
        }
    }

    /// MC68000 timing for `CMP <ea>,Dn`: base 4 byte/word, 6 long (no
    /// register/immediate +2 -- CMP performs no writeback), plus source EA.
    #[inline]
    pub(crate) fn cmp_ea_dn_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        let base = if size == Size::Long { 6 } else { 4 };
        base + self.ea_source_cycles(mode, size)
    }

    /// MC68000 timing for `EOR Dn,<ea>`: data-register destination 4 byte/word,
    /// 8 long; memory destination uses the `Dn,<ea>` read-modify-write timing.
    #[inline]
    pub(crate) fn eor_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        if matches!(mode, AddressingMode::DataDirect(_)) {
            if size == Size::Long { 8 } else { 4 }
        } else {
            self.alu_dn_ea_cycles(mode, size)
        }
    }

    /// MC68000 timing for ADDQ/SUBQ. Data register: 8 long, 4 byte/word.
    /// Address register: 8 (word/long). Memory: 12 long / 8 byte-word plus the
    /// destination read/EA cost.
    #[inline]
    pub(crate) fn addq_subq_cycles(&self, mode: AddressingMode, size: Size) -> i32 {
        match mode {
            AddressingMode::DataDirect(_) => {
                if size == Size::Long {
                    8
                } else {
                    4
                }
            }
            AddressingMode::AddressDirect(_) => 8,
            _ => {
                let base = if size == Size::Long { 12 } else { 8 };
                base + self.ea_source_cycles(mode, size)
            }
        }
    }
}
