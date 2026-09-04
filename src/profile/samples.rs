// SPDX-License-Identifier: GPL-3.0-or-later

//! Precise CPU samples and Bartman's compact m68k unwind table.

use crate::bus::Bus;

pub const IRQ_MARKER: u32 = 0x7fff_ffff;
pub const MAX_CALLSTACK_DEPTH: usize = 16;
pub const REGISTER_COUNT: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactUnwindRow {
    pub cfa_reg: u8,
    pub cfa_offset: i32,
    pub r13_offset: i32,
    pub ra_offset: i32,
}

/// One row per two bytes of program text. The wire format is exactly the
/// vscode-amiga-debug/WinUAE format: three little-endian signed 16-bit
/// codewords, with the CFA register packed into the first word's top nibble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactUnwindTable {
    base: u32,
    rows: Vec<CompactUnwindRow>,
}

impl CompactUnwindTable {
    pub fn decode(base: u32, bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || !bytes.len().is_multiple_of(6) {
            return Err("unwind table must contain one 6-byte row per text word".into());
        }
        let mut rows = Vec::with_capacity(bytes.len() / 6);
        for (index, row) in bytes.chunks_exact(6).enumerate() {
            let cfa = u16::from_le_bytes([row[0], row[1]]);
            let cfa_reg = (cfa >> 12) as u8;
            if !matches!(cfa_reg, 13 | 15) {
                return Err(format!(
                    "unwind row {index} uses CFA register r{cfa_reg}; expected r13 or r15"
                ));
            }
            rows.push(CompactUnwindRow {
                cfa_reg,
                cfa_offset: i32::from(cfa & 0x0fff),
                r13_offset: i32::from(i16::from_le_bytes([row[2], row[3]])),
                ra_offset: i32::from(i16::from_le_bytes([row[4], row[5]])),
            });
        }
        Ok(Self { base, rows })
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    pub fn text_size(&self) -> u32 {
        (self.rows.len() as u32).saturating_mul(2)
    }

    fn contains(&self, pc: u32) -> bool {
        pc >= self.base && pc.wrapping_sub(self.base) < self.text_size()
    }

    fn row(&self, pc: u32) -> Option<CompactUnwindRow> {
        if !self.contains(pc) {
            return None;
        }
        self.rows.get(((pc - self.base) >> 1) as usize).copied()
    }

    fn normalize_pc(&self, pc: u32) -> Option<u32> {
        self.contains(pc).then_some(pc - self.base)
    }

    fn unwind(
        &self,
        mut pc: u32,
        registers: &[u32; REGISTER_COUNT],
        mut read32: impl FnMut(u32) -> u32,
    ) -> Callstack {
        let mut out = Callstack::default();
        if (0x00f8_0000..0x0100_0000).contains(&pc) {
            out.push(pc);
            return out;
        }

        let mut r13 = registers[13];
        let mut r15 = registers[15];
        while out.depth < MAX_CALLSTACK_DEPTH {
            let Some(normalized) = self.normalize_pc(pc) else {
                break;
            };
            out.push(normalized);
            let Some(row) = self.row(pc) else {
                break;
            };
            let cfa_base = match row.cfa_reg {
                13 => r13,
                15 => r15,
                _ => break,
            };
            let new_cfa = cfa_base.wrapping_add_signed(row.cfa_offset);
            let new_pc = read32(new_cfa.wrapping_add_signed(row.ra_offset));
            if row.r13_offset != -1 {
                r13 = read32(new_cfa.wrapping_add_signed(row.r13_offset));
            }
            if new_cfa == r15 || new_pc == pc {
                break;
            }
            r15 = new_cfa;
            pc = new_pc;
        }
        out
    }
}

fn peek_long(bus: &Bus, addr: u32) -> u32 {
    if (0x00df_f000..0x00e0_0000).contains(&addr) {
        return u32::MAX;
    }
    (u32::from(bus.peek_word_any(addr)) << 16) | u32::from(bus.peek_word_any(addr.wrapping_add(2)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqInfo {
    pub level: u8,
    pub vector: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSample {
    pub callstack: [u32; MAX_CALLSTACK_DEPTH],
    pub callstack_depth: usize,
    pub total_cck: u32,
    pub instruction_cck: u32,
    pub bus_wait_cck: u32,
    pub registers: Option<[u32; REGISTER_COUNT]>,
    pub irq: Option<IrqInfo>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Callstack {
    pcs: [u32; MAX_CALLSTACK_DEPTH],
    depth: usize,
}

impl Callstack {
    fn push(&mut self, pc: u32) {
        if self.depth < MAX_CALLSTACK_DEPTH {
            self.pcs[self.depth] = pc;
            self.depth += 1;
        }
    }
}

/// State captured immediately before an instruction or interrupt dispatch.
#[derive(Debug, Clone, Copy)]
pub struct SampleStart {
    pc: u32,
    registers: [u32; REGISTER_COUNT],
    bus_wait_cck: u64,
}

pub struct InstructionSampler {
    unwind: Option<CompactUnwindTable>,
    include_registers: bool,
    pending: Vec<InstructionSample>,
}

impl InstructionSampler {
    pub fn new(unwind: Option<CompactUnwindTable>, include_registers: bool) -> Self {
        Self {
            unwind,
            include_registers,
            pending: Vec::new(),
        }
    }

    pub fn start(
        &self,
        pc: u32,
        registers: [u32; REGISTER_COUNT],
        bus_wait_cck: u64,
    ) -> SampleStart {
        SampleStart {
            pc,
            registers,
            bus_wait_cck,
        }
    }

    pub fn finish_instruction(&mut self, start: SampleStart, instruction_cck: u32, bus: &Bus) {
        let wait = bus.cpu_wait_cck_total().wrapping_sub(start.bus_wait_cck);
        let bus_wait_cck = wait.min(u64::from(u32::MAX)) as u32;
        let total_cck = instruction_cck.saturating_add(bus_wait_cck);
        let mut callstack = Callstack::default();
        if let Some(unwind) = &self.unwind {
            callstack = unwind.unwind(start.pc, &start.registers, |addr| peek_long(bus, addr));
        } else {
            callstack.push(start.pc);
        }
        self.pending.push(InstructionSample {
            callstack: callstack.pcs,
            callstack_depth: callstack.depth,
            total_cck,
            instruction_cck,
            bus_wait_cck,
            registers: self.include_registers.then_some(start.registers),
            irq: None,
        });
    }

    pub fn finish_irq(
        &mut self,
        start: SampleStart,
        instruction_cck: u32,
        irq: IrqInfo,
        bus: &Bus,
    ) {
        let wait = bus.cpu_wait_cck_total().wrapping_sub(start.bus_wait_cck);
        let bus_wait_cck = wait.min(u64::from(u32::MAX)) as u32;
        self.pending.push(InstructionSample {
            callstack: [IRQ_MARKER; MAX_CALLSTACK_DEPTH],
            callstack_depth: 1,
            total_cck: instruction_cck.saturating_add(bus_wait_cck),
            instruction_cck,
            bus_wait_cck,
            registers: self.include_registers.then_some(start.registers),
            irq: Some(irq),
        });
    }

    pub fn take(&mut self) -> Vec<InstructionSample> {
        std::mem::take(&mut self.pending)
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_signed_offsets_without_narrowing_the_internal_form() {
        let bytes = [0x04, 0xf0, 0xf8, 0xff, 0xfc, 0xff];
        let table = CompactUnwindTable::decode(0x1000, &bytes).unwrap();
        assert_eq!(
            table.rows[0],
            CompactUnwindRow {
                cfa_reg: 15,
                cfa_offset: 4,
                r13_offset: -8,
                ra_offset: -4,
            }
        );
    }

    #[test]
    fn rejects_non_stack_cfa_registers() {
        let err = CompactUnwindTable::decode(0x1000, &[4, 0, 0, 0, 0, 0]).unwrap_err();
        assert!(err.contains("r0"), "{err}");
    }

    #[test]
    fn synthetic_unwind_uses_full_twelve_bit_cfa_offset() {
        // CFA = A7 + 0x800 (not a sign-extended -0x800); RA is at CFA-4.
        let bytes = [0x00, 0xf8, 0xff, 0xff, 0xfc, 0xff];
        let table = CompactUnwindTable::decode(0x1000, &bytes).unwrap();
        let mut registers = [0; REGISTER_COUNT];
        registers[15] = 0x2000;
        let stack = table.unwind(0x1000, &registers, |addr| {
            assert_eq!(addr, 0x27fc);
            0x9000
        });
        assert_eq!(stack.depth, 1, "the external return address stops the walk");
        assert_eq!(stack.pcs[0], 0);
    }
}
