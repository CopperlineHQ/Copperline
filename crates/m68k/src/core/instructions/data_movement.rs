//! Data movement instructions.
//!
//! MOVE, MOVEA, MOVEM, LEA, PEA, EXG, LINK, UNLK

use crate::core::cpu::CpuCore;
use crate::core::ea::{AddressingMode, EaResult};
use crate::core::memory::AddressBus;
use crate::core::types::{CpuType, Size};

impl CpuCore {
    /// Execute MOVE instruction.
    ///
    /// MOVE <ea>, <ea>
    #[inline]
    pub fn exec_move<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        src_mode: AddressingMode,
        dst_mode: AddressingMode,
    ) -> i32 {
        // Read source value
        let value = self.read_ea(bus, src_mode, size);

        // Write to destination. The 68000 sequences its final prefetch
        // differently per destination mode:
        // - predecrement: prefetch BEFORE the write (the write is last)
        // - absolute long WITH A MEMORY SOURCE: the address low word is
        //   consumed without its np; both remaining prefetches happen AFTER
        //   the write (Class 2). With a register or immediate source the
        //   68000 uses the normal Class 1 order instead (pasti 68kPrefetch:
        //   "if the source operand is a data or address register, or
        //   immediate, then the behavior is the same as other MOVE
        //   variants") - the order matters beyond timing, because the
        //   interrupt-decision IPL sample rides the final access: a Class 1
        //   MOVE #x,INTREQ polls right after its own write (too early to
        //   see the interrupt it just raised), while the misapplied Class 2
        //   tail let bus contention push the poll past the IPL pipe and the
        //   instruction recognized its own interrupt one boundary early
        //   (eon's scene-player yield raise).
        // - everything else: write, then the final prefetch (Class 1)
        let memory_source = !matches!(
            src_mode,
            AddressingMode::DataDirect(_)
                | AddressingMode::AddressDirect(_)
                | AddressingMode::Immediate
        );
        let class2_abs_long = memory_source;
        if self.cpu_type == CpuType::M68000 {
            match dst_mode {
                AddressingMode::PreDecrement(_) => {
                    let ea = self.resolve_ea(bus, dst_mode, size);
                    // A MOVE destination predecrement does not pay the
                    // 2-clock address-computation penalty (it overlaps the
                    // final prefetch); cancel resolve_ea's charge.
                    self.pending_sync_clocks = self.pending_sync_clocks.saturating_sub(2);
                    self.top_up_prefetch(bus);
                    // MOVE to -(An) behaves like an RMW instruction (pasti
                    // class 0): the IPL poll rides this prefetch and the
                    // trailing write does not re-latch it.
                    self.ipl_poll_point(bus);
                    if let EaResult::Memory(addr) = ea {
                        match size {
                            Size::Byte => self.write_8(bus, addr, value as u8),
                            Size::Word => self.write_16(bus, addr, value as u16),
                            // Long predecrement writes descend: low word at
                            // the higher address first, then the high word.
                            Size::Long => {
                                self.write_16(
                                    bus,
                                    addr.wrapping_add(2),
                                    (value & 0xFFFF) as u16,
                                );
                                self.write_16(bus, addr, (value >> 16) as u16);
                            }
                        }
                    }
                }
                AddressingMode::PostIncrement(_) => {
                    // MOVE to (An)+ polls IPL during the destination write
                    // itself, for every source and size (Moira execMove3
                    // writeOp<POLL>); the final prefetch that follows does
                    // not re-latch. Long destinations write high word then
                    // low word with the poll riding the low word, so the
                    // write is split explicitly.
                    let ea = self.resolve_ea(bus, dst_mode, size);
                    if let EaResult::Memory(addr) = ea {
                        self.write_move_dest_68000(bus, addr, size, value);
                    }
                    self.ipl_poll_point(bus);
                }
                AddressingMode::AddressIndirect(_)
                | AddressingMode::Displacement(_)
                | AddressingMode::Index(_)
                    if size == Size::Long && !memory_source =>
                {
                    // MOVE.L with a register or immediate source to (An),
                    // d16(An) or d8(An,Xn) polls IPL during the write's low
                    // word (Moira execMove2/5/6 writeOp<POLL> on the long
                    // branch); with a memory source or smaller sizes the
                    // poll rides the final prefetch instead (the default).
                    let ea = self.resolve_ea(bus, dst_mode, size);
                    if let EaResult::Memory(addr) = ea {
                        self.write_move_dest_68000(bus, addr, size, value);
                    }
                    self.ipl_poll_point(bus);
                }
                AddressingMode::AbsoluteLong if class2_abs_long => {
                    // Consume the address high word normally, the low word
                    // without its accompanying prefetch.
                    let hi = self.read_imm_16(bus) as u32;
                    self.consume_without_prefetch = true;
                    let lo = self.read_imm_16(bus) as u32;
                    self.consume_without_prefetch = false;
                    let addr = (hi << 16) | lo;
                    match size {
                        Size::Byte => self.write_8(bus, addr, value as u8),
                        Size::Word => self.write_16(bus, addr, value as u16),
                        Size::Long => self.write_32(bus, addr, value),
                    }
                    // The deferred np + final prefetch follow via the
                    // end-of-instruction top-up.
                }
                _ => {
                    self.write_ea(bus, dst_mode, size, value);
                }
            }
        } else {
            self.write_ea(bus, dst_mode, size, value);
        }

        // Set flags
        self.set_logic_flags(value, size);

        // MC68000: base 4 + source-fetch EA + destination-write EA.
        if self.cpu_type == CpuType::M68000 {
            4 + self.ea_source_cycles(src_mode, size) + self.ea_dest_cycles(dst_mode, size)
        } else {
            // 020+: the flat 4 made every MOVE cost the same regardless of
            // operand, so register moves ran a cycle slow and memory reads a
            // couple fast. Model the 020 data-return latency on a memory
            // source. Values are pre-scale (scale_cycles_for_cpu_type applies
            // 5/8) and calibrated to the cycle-exact A1200/FS-UAE reference:
            // Dn,Dm = 2; (An),Dn word read = 6; Dn,(An) write stays bus-bound.
            // (Write-posting, which the bus model lacks, is handled there.)
            let mut c = 2;
            if !src_mode.is_register_direct() {
                c += if size == Size::Long { 11 } else { 7 };
            }
            c
        }
    }

    /// MOVE destination write on the 68000, split into its word bus cycles:
    /// high word first, then low word (long destinations). Splitting keeps
    /// the host's per-access IPL samples, so a poll point placed right
    /// after this write pins the sample taken at the FINAL word's start
    /// (Moira's writeOp<POLL> polls before the low word of a long write).
    fn write_move_dest_68000<B: AddressBus>(
        &mut self,
        bus: &mut B,
        addr: u32,
        size: Size,
        value: u32,
    ) {
        match size {
            Size::Byte => self.write_8(bus, addr, value as u8),
            Size::Word => self.write_16(bus, addr, value as u16),
            Size::Long => {
                self.write_16(bus, addr, (value >> 16) as u16);
                self.write_16(bus, addr.wrapping_add(2), value as u16);
            }
        }
    }

    /// Execute MOVEA instruction.
    ///
    /// MOVEA <ea>, An (no flags affected)
    pub fn exec_movea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        src_mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        let value = self.read_ea(bus, src_mode, size);

        // Sign extend word to long for MOVEA.W
        let value = if size == Size::Word {
            value as i16 as i32 as u32
        } else {
            value
        };

        // 68000/68010 MOVEA sequences the final prefetch before the address
        // register update (Moira execMovea: prefetch<POLL>, then writeA).
        if self.prefetch_enabled() {
            self.top_up_prefetch(bus);
            self.ipl_poll_point(bus);
        }
        self.set_a(dst_reg, value);
        // MC68000: MOVEA base 4 + source-fetch EA (destination An is free).
        if self.cpu_type == CpuType::M68000 {
            4 + self.ea_source_cycles(src_mode, size)
        } else {
            4
        }
    }

    /// Execute LEA instruction.
    ///
    /// LEA <ea>, An
    pub fn exec_lea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        src_mode: AddressingMode,
        dst_reg: usize,
    ) -> i32 {
        // Get effective address (don't read from it)
        let ea = self.get_ea_address(bus, src_mode, Size::Long);
        // Indexed modes spend 2 more internal clocks after the extension
        // fetch, before the final prefetch.
        if matches!(
            src_mode,
            AddressingMode::Index(_) | AddressingMode::PcIndex
        ) {
            self.internal_cycles(2);
        }
        self.set_a(dst_reg, ea);
        4
    }

    /// Execute PEA instruction.
    ///
    /// PEA <ea>
    pub fn exec_pea<B: AddressBus>(&mut self, bus: &mut B, src_mode: AddressingMode) -> i32 {
        let ea = self.get_ea_address(bus, src_mode, Size::Long);
        // Indexed modes spend 2 more internal clocks after the extension
        // fetch, before the final prefetch.
        if matches!(
            src_mode,
            AddressingMode::Index(_) | AddressingMode::PcIndex
        ) {
            self.internal_cycles(2);
        }
        if self.cpu_type == CpuType::M68000
            && matches!(
                src_mode,
                AddressingMode::AbsoluteShort | AddressingMode::AbsoluteLong
            )
        {
            // 68000 absolute-mode PEA pushes FIRST and prefetches last
            // (Moira execPea abs branch: push, then prefetch<POLL>); the
            // IPL poll rides that final prefetch, the default sample.
            self.push_32(bus, ea);
            self.top_up_prefetch(bus);
        } else {
            // Other modes: the final prefetch precedes the push and
            // carries the IPL poll (Moira: POLL_IPL/prefetch<POLL>, then
            // push); the push does not re-latch the boundary sample.
            self.top_up_prefetch(bus);
            self.ipl_poll_point(bus);
            self.push_32(bus, ea);
        }
        12
    }

    /// Execute EXG instruction.
    ///
    /// EXG Rx, Ry
    pub fn exec_exg<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        let rx = ((opcode >> 9) & 7) as usize;
        let ry = (opcode & 7) as usize;
        let mode = (opcode >> 3) & 0x1F;

        match mode {
            0x08 => {
                // EXG Dx, Dy
                let tmp = self.d(rx);
                self.set_d(rx, self.d(ry));
                self.set_d(ry, tmp);
            }
            0x09 => {
                // EXG Ax, Ay
                let tmp = self.a(rx);
                self.set_a(rx, self.a(ry));
                self.set_a(ry, tmp);
            }
            0x11 => {
                // EXG Dx, Ay
                let tmp = self.d(rx);
                self.set_d(rx, self.a(ry));
                self.set_a(ry, tmp);
            }
            _ => {}
        }
        self.top_up_prefetch(bus);
        self.ipl_poll_point(bus);
        self.internal_cycles(2);
        self.flush_sync(bus);
        6
    }

    /// Execute LINK instruction.
    ///
    /// LINK An, #<displacement>
    /// The 68040 performs LINK's A7 predecrement before reading the source
    /// register; the 68000-030 and the 68060 read the source first.
    fn link_predecrements_first(&self) -> bool {
        matches!(
            self.cpu_type,
            crate::core::types::CpuType::M68EC040
                | crate::core::types::CpuType::M68LC040
                | crate::core::types::CpuType::M68040
        )
    }

    pub fn exec_link<B: AddressBus>(&mut self, bus: &mut B, reg: usize) -> i32 {
        // 68000 bus order: the displacement word is consumed (with its
        // prefetch) BEFORE the An push.
        let disp = self.read_imm_16(bus) as i16 as i32;

        // Push An. The 68040 (alone) decrements A7 before reading the
        // source register, so LINK A7 pushes the decremented value there;
        // every other generation pushes the original.
        let an = if self.link_predecrements_first() && reg == 7 {
            self.a(7).wrapping_sub(4)
        } else {
            self.a(reg)
        };
        self.push_32(bus, an);
        // LINK's IPL poll point sits right before the An push (Moira:
        // POLL_IPL then push), i.e. the sample taken at the push's first
        // word; the final prefetch does not re-latch it.
        self.ipl_poll_point(bus);

        // An = SP
        self.set_a(reg, self.dar[15]);

        // SP += displacement (16-bit)
        self.dar[15] = (self.dar[15] as i32).wrapping_add(disp) as u32;

        16
    }

    /// Execute LINK.L instruction (68020+).
    ///
    /// LINK.L An, #<displacement> (32-bit displacement)
    pub fn exec_link_long<B: AddressBus>(&mut self, bus: &mut B, reg: usize) -> i32 {
        // Push An (68040 A7 ordering quirk as in exec_link)
        let an = if self.link_predecrements_first() && reg == 7 {
            self.a(7).wrapping_sub(4)
        } else {
            self.a(reg)
        };
        self.push_32(bus, an);

        // An = SP
        self.set_a(reg, self.dar[15]);

        // SP += displacement (32-bit)
        let disp = self.read_imm_32(bus) as i32;
        self.dar[15] = (self.dar[15] as i32).wrapping_add(disp) as u32;

        16
    }

    /// Execute UNLK instruction.
    ///
    /// UNLK An
    pub fn exec_unlk<B: AddressBus>(&mut self, bus: &mut B, reg: usize) -> i32 {
        // SP = An
        self.dar[15] = self.a(reg);

        // Pop An
        let value = if self.prefetch_enabled() {
            // The 68000/68010 poll IPL before the LOW word of the stack
            // read (Moira readOp<.., Long, POLL>), so the read is split
            // into its two word cycles and the sample taken at the second
            // word's start is held through the final prefetch.
            let sp = self.dar[15];
            let hi = self.read_16(bus, sp) as u32;
            let lo = self.read_16(bus, sp.wrapping_add(2)) as u32;
            self.ipl_poll_point(bus);
            self.dar[15] = sp.wrapping_add(4);
            (hi << 16) | lo
        } else {
            self.pull_32(bus)
        };
        self.set_a(reg, value);

        12
    }

    /// Execute MOVEM instruction (register to memory).
    ///
    /// MOVEM <register list>, <ea>
    pub fn exec_movem_to_mem<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
        mask: u16,
    ) -> i32 {
        let mut count = 0;

        // For predecrement mode, bit order is reversed (A7..A0, D7..D0)
        let is_predec = matches!(mode, AddressingMode::PreDecrement(_));

        // Get starting address
        let mut addr = match &mode {
            AddressingMode::PreDecrement(reg) => self.a(*reg as usize),
            _ => self.get_ea_address(bus, mode, size),
        };

        if is_predec {
            // When the base register itself is in the list, the 68020+
            // store its initial value minus one transfer size; the
            // 68000/010 store the plain initial value. The register is
            // only written back once, after the loop.
            let base_reg = match mode {
                AddressingMode::PreDecrement(reg) => 8 + reg as usize,
                _ => unreachable!(),
            };
            let base_adjust = if self.is_pre_68020 { 0 } else { size.bytes() };
            // Write in reverse order: A7..A0, D7..D0
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let reg_idx = 15 - i; // Reverse: bit 0 = A7, bit 15 = D0
                    let mut value = self.dar[reg_idx];
                    if reg_idx == base_reg {
                        value = value.wrapping_sub(base_adjust);
                    }
                    match size {
                        Size::Word => {
                            addr = addr.wrapping_sub(2);
                            self.write_16(bus, addr, value as u16);
                        }
                        Size::Long if self.cpu_type == CpuType::M68000 => {
                            addr = addr.wrapping_sub(2);
                            self.write_16(bus, addr, (value & 0xFFFF) as u16);
                            addr = addr.wrapping_sub(2);
                            self.write_16(bus, addr, (value >> 16) as u16);
                        }
                        Size::Long => {
                            addr = addr.wrapping_sub(4);
                            self.write_32(bus, addr, value);
                        }
                        _ => {}
                    }
                    count += 1;
                }
            }
            // Update address register
            if let AddressingMode::PreDecrement(reg) = mode {
                self.set_a(reg as usize, addr);
            }
        } else {
            // Normal order: D0..D7, A0..A7
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let value = self.dar[i];
                    match size {
                        Size::Word => self.write_16(bus, addr, value as u16),
                        Size::Long => self.write_32(bus, addr, value),
                        _ => {}
                    }
                    addr = addr.wrapping_add(size.bytes());
                    count += 1;
                }
            }
        }

        {
            let base = 8 + count * if size == Size::Long { 8 } else { 4 };
            if self.cpu_type == CpuType::M68000 {
                base + self.movem_ea_calc_cycles(mode)
            } else {
                base
            }
        }
    }

    /// Execute MOVEM instruction (memory to register).
    ///
    /// MOVEM <ea>, <register list>
    pub fn exec_movem_to_reg<B: AddressBus>(
        &mut self,
        bus: &mut B,
        size: Size,
        mode: AddressingMode,
        mask: u16,
    ) -> i32 {
        let mut count = 0;
        let is_predec = matches!(mode, AddressingMode::PreDecrement(_));

        // Establish starting address depending on addressing mode.
        let mut addr = match &mode {
            AddressingMode::PostIncrement(reg) => self.a(*reg as usize),
            AddressingMode::PreDecrement(reg) => self.a(*reg as usize),
            _ => self.get_ea_address(bus, mode, size),
        };

        if is_predec {
            // Predecrement source: reverse register order A7..A0, D7..D0
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let reg_idx = 15 - i;
                    addr = addr.wrapping_sub(size.bytes());
                    let value = match size {
                        Size::Word => self.read_16(bus, addr) as i16 as i32 as u32,
                        Size::Long => self.read_32(bus, addr),
                        _ => 0,
                    };
                    self.dar[reg_idx] = value;
                    count += 1;
                }
            }
            // Update address register after all reads
            if let AddressingMode::PreDecrement(reg) = mode {
                self.set_a(reg as usize, addr);
            }
        } else {
            if self.cpu_type == CpuType::M68000 && size == Size::Long {
                let _ = self.read_16(bus, addr);
            }

            // Normal order: D0..D7, A0..A7
            for i in 0..16 {
                if mask & (1 << i) != 0 {
                    let value = match size {
                        Size::Word => self.read_16(bus, addr) as i16 as i32 as u32,
                        Size::Long => self.read_32(bus, addr),
                        _ => 0,
                    };
                    self.dar[i] = value;
                    addr = addr.wrapping_add(size.bytes());
                    count += 1;
                }
            }
            // Update address register for postincrement
            if let AddressingMode::PostIncrement(reg) = mode {
                self.set_a(reg as usize, addr);
            }

            // 68000 MOVEM memory-to-register has one discarded word read:
            // before long transfers, after word transfers.
            if self.cpu_type == CpuType::M68000 && size == Size::Word {
                let _ = self.read_16(bus, addr);
            }
        }

        {
            let base = 12 + count * if size == Size::Long { 8 } else { 4 };
            if self.cpu_type == CpuType::M68000 {
                base + self.movem_ea_calc_cycles(mode)
            } else {
                base
            }
        }
    }

    /// Execute SWAP instruction.
    ///
    /// SWAP Dn
    pub fn exec_swap<B: AddressBus>(&mut self, bus: &mut B, reg: usize) -> i32 {
        let value = self.d(reg);
        let result = value.rotate_right(16);
        self.top_up_prefetch(bus);
        self.ipl_poll_point(bus);
        self.set_d(reg, result);

        self.set_logic_flags(result, Size::Long);
        4
    }

    // ========== Helper Methods ==========

    /// Read value from effective address.
    #[inline]
    pub fn read_ea<B: AddressBus>(&mut self, bus: &mut B, mode: AddressingMode, size: Size) -> u32 {
        match self.resolve_ea(bus, mode, size) {
            EaResult::DataReg(reg) => self.d(reg as usize) & size.mask(),
            EaResult::AddrReg(reg) => self.a(reg as usize) & size.mask(),
            EaResult::Memory(addr) => match size {
                Size::Byte => self.read_8(bus, addr) as u32,
                Size::Word => self.read_16(bus, addr) as u32,
                Size::Long => self.read_32(bus, addr),
            },
            // Immediate data was already consumed from the instruction stream
            // by resolve_ea (prefetch queue on 68000).
            EaResult::Immediate(value) => value & size.mask(),
        }
    }

    /// Write value to effective address.
    #[inline]
    pub fn write_ea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        size: Size,
        value: u32,
    ) {
        match self.resolve_ea(bus, mode, size) {
            EaResult::DataReg(reg) => {
                let reg = reg as usize;
                match size {
                    Size::Byte => {
                        self.dar[reg] = (self.dar[reg] & 0xFFFFFF00) | (value & 0xFF);
                    }
                    Size::Word => {
                        self.dar[reg] = (self.dar[reg] & 0xFFFF0000) | (value & 0xFFFF);
                    }
                    Size::Long => {
                        self.dar[reg] = value;
                    }
                }
            }
            EaResult::AddrReg(reg) => {
                // Address registers always get full 32-bit value
                self.dar[8 + reg as usize] = value;
            }
            EaResult::Memory(addr) => match size {
                Size::Byte => self.write_8(bus, addr, value as u8),
                Size::Word => self.write_16(bus, addr, value as u16),
                Size::Long => self.write_32(bus, addr, value),
            },
            EaResult::Immediate(_) => {
                // Can't write to immediate - should not happen
            }
        }
    }

    /// Read value from an already-resolved effective address.
    pub fn read_resolved_ea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        ea: EaResult,
        size: Size,
    ) -> u32 {
        match ea {
            EaResult::DataReg(reg) => self.d(reg as usize) & size.mask(),
            EaResult::AddrReg(reg) => self.a(reg as usize) & size.mask(),
            EaResult::Memory(addr) => match size {
                Size::Byte => self.read_8(bus, addr) as u32,
                Size::Word => self.read_16(bus, addr) as u32,
                Size::Long => self.read_32(bus, addr),
            },
            // Immediate data was already consumed from the instruction stream
            // by resolve_ea (prefetch queue on 68000).
            EaResult::Immediate(value) => value & size.mask(),
        }
    }

    /// Write value to an already-resolved effective address.
    ///
    /// This is the read-modify-write writeback path (the destination was
    /// resolved once and usually read first). On the 68000, RMW instructions
    /// perform their final prefetch BEFORE the writeback (unlike MOVE, whose
    /// write precedes the final prefetch), so the memory arm tops the
    /// prefetch queue up first.
    ///
    /// The interrupt-decision IPL sample rides the WRITEBACK here (ADDI/
    /// SUBI and MOVE from SR poll during their write; Moira writeOp POLL).
    /// Most RMW instructions instead poll during the preceding prefetch --
    /// those use `write_resolved_ea_np_poll`.
    pub fn write_resolved_ea<B: AddressBus>(
        &mut self,
        bus: &mut B,
        ea: EaResult,
        size: Size,
        value: u32,
    ) {
        self.write_resolved_ea_impl(bus, ea, size, value, false);
    }

    /// RMW writeback whose IPL poll point is the final prefetch BEFORE the
    /// write (the 68000 logic/shift/bit/CLR/NEG/NOT/ADDQ/Scc class: Moira
    /// prefetch<POLL> then writeOp). The writeback accesses do not re-latch
    /// the boundary sample, so an interrupt that rises while the write is
    /// arbitrating is not taken until one instruction later.
    pub fn write_resolved_ea_np_poll<B: AddressBus>(
        &mut self,
        bus: &mut B,
        ea: EaResult,
        size: Size,
        value: u32,
    ) {
        self.write_resolved_ea_impl(bus, ea, size, value, true);
    }

    fn write_resolved_ea_impl<B: AddressBus>(
        &mut self,
        bus: &mut B,
        ea: EaResult,
        size: Size,
        value: u32,
        np_poll: bool,
    ) {
        match ea {
            EaResult::DataReg(reg) => {
                let reg = reg as usize;
                match size {
                    Size::Byte => self.dar[reg] = (self.dar[reg] & 0xFFFFFF00) | (value & 0xFF),
                    Size::Word => self.dar[reg] = (self.dar[reg] & 0xFFFF0000) | (value & 0xFFFF),
                    Size::Long => self.dar[reg] = value,
                }
            }
            EaResult::AddrReg(reg) => self.dar[8 + reg as usize] = value,
            EaResult::Memory(addr) => {
                self.top_up_prefetch(bus);
                if np_poll {
                    self.ipl_poll_point(bus);
                }
                match size {
                    Size::Byte => self.write_8(bus, addr, value as u8),
                    Size::Word => self.write_16(bus, addr, value as u16),
                    // 16-bit-bus CPUs (68000/68010/SCC68070) run a long RMW
                    // writeback as two word cycles, low word first, then high
                    // word (unlike MOVE.L destinations, which write high
                    // first). The 32-bit-bus CPUs issue one aligned long
                    // access; splitting it breaks MMU fault reporting (the
                    // access-error frame would describe a word-sized half, so
                    // a handler that completes the writeback -- Linux
                    // do_040writebacks -- completes only half the store).
                    Size::Long
                        if matches!(
                            self.cpu_type,
                            CpuType::M68000 | CpuType::M68010 | CpuType::SCC68070
                        ) =>
                    {
                        self.write_16(bus, addr.wrapping_add(2), (value & 0xFFFF) as u16);
                        self.write_16(bus, addr, (value >> 16) as u16);
                    }
                    Size::Long => self.write_32(bus, addr, value),
                }
            }
            EaResult::Immediate(_) => {
                // Can't write to immediate
            }
        }
    }

    /// Get effective address without reading.
    pub fn get_ea_address<B: AddressBus>(
        &mut self,
        bus: &mut B,
        mode: AddressingMode,
        size: Size,
    ) -> u32 {
        match self.resolve_ea(bus, mode, size) {
            EaResult::Memory(addr) => addr,
            // Immediate is not a valid control/address EA (LEA/PEA/JMP/JSR).
            EaResult::Immediate(_) | EaResult::DataReg(_) | EaResult::AddrReg(_) => 0,
        }
    }

    /// Set N, Z flags based on result. Clear V, C.
    pub fn set_logic_flags(&mut self, value: u32, size: Size) {
        let msb = size.msb_mask();
        self.n_flag = if value & msb != 0 { 0x80 } else { 0 };
        self.not_z_flag = value & size.mask();
        self.v_flag = 0;
        self.c_flag = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        ReadWord(u32),
        WriteWord(u32, u16),
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

        fn read_long(&mut self, address: u32) -> u32 {
            (u32::from(self.read_word(address)) << 16)
                | u32::from(self.read_word(address.wrapping_add(2)))
        }

        fn write_byte(&mut self, _address: u32, _value: u8) {}

        fn write_word(&mut self, address: u32, value: u16) {
            self.events.push(Event::WriteWord(address, value));
        }

        fn write_long(&mut self, address: u32, value: u32) {
            self.write_word(address, (value >> 16) as u16);
            self.write_word(address.wrapping_add(2), value as u16);
        }

        fn sync(&mut self, cpu_clocks: u32) {
            self.events.push(Event::Sync(cpu_clocks));
        }

        fn ipl_hold_sample(&mut self) {
            self.events.push(Event::IplHold);
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
    fn m68000_swap_prefetches_before_register_update() {
        let mut cpu = m68000_cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x1234_ABCD;

        let cycles = cpu.exec_swap(&mut bus, 0);

        assert_eq!(cycles, 4);
        assert_eq!(cpu.dar[0], 0xABCD_1234);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(bus.events, vec![Event::ReadWord(0x2002), Event::IplHold]);
    }

    #[test]
    fn m68000_movea_prefetches_before_address_register_update() {
        let mut cpu = m68000_cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0xffff_8000;

        let cycles = cpu.exec_movea(&mut bus, Size::Word, AddressingMode::DataDirect(0), 1);

        assert_eq!(cycles, 4);
        assert_eq!(cpu.dar[9], 0xffff_8000);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(bus.events, vec![Event::ReadWord(0x2002), Event::IplHold]);
    }

    #[test]
    fn m68000_exg_prefetches_before_internal_sync() {
        let mut cpu = m68000_cpu_with_one_prefetch_word();
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x1111_2222;
        cpu.dar[1] = 0x3333_4444;

        let cycles = cpu.exec_exg(&mut bus, 0xC141);

        assert_eq!(cycles, 6);
        assert_eq!(cpu.dar[0], 0x3333_4444);
        assert_eq!(cpu.dar[1], 0x1111_2222);
        assert_eq!(cpu.prefetch_count, 2);
        assert_eq!(cpu.pending_sync_clocks, 0);
        assert_eq!(
            bus.events,
            vec![Event::ReadWord(0x2002), Event::IplHold, Event::Sync(2)]
        );
    }

    #[test]
    fn m68000_movem_long_predecrement_writes_low_word_before_high_word() {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        let mut bus = TraceBus::default();
        cpu.dar[0] = 0x1122_3344;
        cpu.dar[10] = 0x1000;

        let cycles =
            cpu.exec_movem_to_mem(&mut bus, Size::Long, AddressingMode::PreDecrement(2), 0x8000);

        assert_eq!(cycles, 16);
        assert_eq!(cpu.dar[10], 0x0FFC);
        assert_eq!(
            bus.events,
            vec![
                Event::WriteWord(0x0FFE, 0x3344),
                Event::WriteWord(0x0FFC, 0x1122),
            ]
        );
    }

    #[test]
    fn m68000_movem_long_memory_to_register_dummies_before_first_transfer() {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        let mut bus = TraceBus::default();
        cpu.dar[10] = 0x1000;

        let cycles =
            cpu.exec_movem_to_reg(&mut bus, Size::Long, AddressingMode::AddressIndirect(2), 0x0001);

        assert_eq!(cycles, 20);
        assert_eq!(
            bus.events,
            vec![
                Event::ReadWord(0x1000),
                Event::ReadWord(0x1000),
                Event::ReadWord(0x1002),
            ]
        );
    }

    #[test]
    fn m68000_movem_word_memory_to_register_dummies_after_last_transfer() {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        let mut bus = TraceBus::default();
        cpu.dar[10] = 0x1000;

        let cycles =
            cpu.exec_movem_to_reg(&mut bus, Size::Word, AddressingMode::AddressIndirect(2), 0x0003);

        assert_eq!(cycles, 20);
        assert_eq!(
            bus.events,
            vec![
                Event::ReadWord(0x1000),
                Event::ReadWord(0x1002),
                Event::ReadWord(0x1004),
            ]
        );
    }
}
