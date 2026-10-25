//! 68020+ Bit Field instructions.
//!
//! Implements: BFTST, BFEXTU, BFEXTS, BFCHG, BFCLR, BFFFO, BFSET, BFINS
//!
//! Encoding reference (Musashi-style):
//! - Primary opcode: 1110 1ooo 11 mmm rrr  (group E, memory-form, bits 7..6 == 11)
//! - Extension word:
//!   - bits 15..12: data register (for BFEXT*/BFFFO dest, BFINS source; ignored otherwise)
//!   - bit 11: offset is dynamic (Dn) if 1, else immediate (5-bit)
//!   - bits 10..6: offset immediate (if bit11=0) OR offset reg (low 3 bits) (if bit11=1)
//!   - bit 5: width is dynamic (Dn) if 1, else immediate (5-bit)
//!   - bits 4..0: width immediate (if bit5=0) OR width reg (low 3 bits) (if bit5=1)
//!
//! Bit numbering is MSB-first (bit 0 = msb of operand).

use crate::core::cpu::CpuCore;
use crate::core::ea::AddressingMode;
use crate::core::memory::AddressBus;
use crate::core::types::Size;

#[derive(Clone, Copy, Debug)]
struct BitFieldSpec {
    reg: usize,
    offset: u32, // raw bit offset (immediate is 5-bit, dynamic uses full Dn value for memory operands)
    width: u32,  // 1..=32
}

impl CpuCore {
    /// Execute a decoded 68020+ bit-field instruction.
    ///
    /// The extension word supplies the dynamic/immediate offset and width,
    /// while `opcode` identifies the operation and effective address.
    pub fn exec_bitfield<B: AddressBus>(&mut self, bus: &mut B, opcode: u16) -> i32 {
        // Read extension word first (before EA extension words).
        let ext = self.read_imm_16(bus);
        let spec = self.decode_bitfield_spec(bus, ext);

        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        let mode = match AddressingMode::decode(ea_mode, ea_reg) {
            Some(m) => m,
            None => return self.take_exception(bus, 4),
        };

        // Operation selector: bits 11..8 (0x8..0xF).
        let op = ((opcode >> 8) & 0xF) as u8;

        // Read the field (from reg or memory), then apply op.
        match mode {
            AddressingMode::DataDirect(dn) => {
                let reg = dn as usize;
                let orig = self.d(reg);
                let reg_offset = spec.offset & 31;
                let field = bf_extract_reg_msb0(orig, reg_offset, spec.width);

                match op {
                    0x8 => {
                        // BFTST
                        self.set_bitfield_flags(field, spec.width);
                        8
                    }
                    0x9 => {
                        // BFEXTU
                        self.set_d(spec.reg, field);
                        self.set_bitfield_flags(field, spec.width);
                        8
                    }
                    0xA => {
                        // BFCHG
                        self.set_bitfield_flags(field, spec.width);
                        let mask = bf_mask(spec.width);
                        let new_field = field ^ mask;
                        let newv = bf_insert_reg_msb0(orig, reg_offset, spec.width, new_field);
                        self.set_d(reg, newv);
                        8
                    }
                    0xB => {
                        // BFEXTS
                        let signed = bf_sign_extend(field, spec.width);
                        self.set_d(spec.reg, signed);
                        self.set_bitfield_flags(field, spec.width);
                        8
                    }
                    0xC => {
                        // BFCLR
                        self.set_bitfield_flags(field, spec.width);
                        let newv = bf_insert_reg_msb0(orig, reg_offset, spec.width, 0);
                        self.set_d(reg, newv);
                        8
                    }
                    0xD => {
                        // BFFFO
                        let (pos, z) = bf_find_first_one(field, spec.width, spec.offset);
                        self.set_d(spec.reg, pos);
                        self.set_bitfield_flags(field, spec.width);
                        if z {
                            self.not_z_flag = 0;
                        }
                        8
                    }
                    0xE => {
                        // BFSET
                        self.set_bitfield_flags(field, spec.width);
                        let mask = bf_mask(spec.width);
                        let newv = bf_insert_reg_msb0(orig, reg_offset, spec.width, mask);
                        self.set_d(reg, newv);
                        8
                    }
                    0xF => {
                        // BFINS (insert from Dn spec.reg)
                        let src = self.d(spec.reg);
                        let src_field = src & bf_mask(spec.width);
                        self.set_bitfield_flags(src_field, spec.width);
                        let newv = bf_insert_reg_msb0(orig, reg_offset, spec.width, src_field);
                        self.set_d(reg, newv);
                        8
                    }
                    _ => self.take_exception(bus, 4),
                }
            }
            _ => {
                // Memory EA
                if mode.is_register_direct() || matches!(mode, AddressingMode::Immediate) {
                    return self.take_exception(bus, 4);
                }

                let base = self.get_ea_address(bus, mode, Size::Byte);
                // For memory operands, a *dynamic* bit offset uses the full Dn value.
                // The byte address is advanced by offset/8 (arithmetic), and the remaining
                // bit offset is offset % 8.
                let byte_disp = ((spec.offset as i32) >> 3) as u32;
                let start_addr = base.wrapping_add(byte_disp);
                let bit_in_byte = spec.offset & 7;

                let (field, mut window, bytes_len) =
                    bf_extract_mem_window_msb0(self, bus, start_addr, bit_in_byte, spec.width);
                // Retain the span for the 020 timing model: the MC68020UM
                // bills a five-byte field one operand cycle above a field
                // within four bytes.
                self.bitfield_mem_wide_span = bytes_len == 5;

                match op {
                    0x8 => {
                        // BFTST
                        self.set_bitfield_flags(field, spec.width);
                        12
                    }
                    0x9 => {
                        // BFEXTU
                        self.set_d(spec.reg, field);
                        self.set_bitfield_flags(field, spec.width);
                        12
                    }
                    0xA => {
                        // BFCHG
                        self.set_bitfield_flags(field, spec.width);
                        let mask =
                            (bf_mask(spec.width) as u64) << (40 - (bit_in_byte + spec.width));
                        window ^= mask;
                        bf_store_mem_window(self, bus, start_addr, window, bytes_len);
                        12
                    }
                    0xB => {
                        // BFEXTS
                        let signed = bf_sign_extend(field, spec.width);
                        self.set_d(spec.reg, signed);
                        self.set_bitfield_flags(field, spec.width);
                        12
                    }
                    0xC => {
                        // BFCLR
                        self.set_bitfield_flags(field, spec.width);
                        let mask =
                            (bf_mask(spec.width) as u64) << (40 - (bit_in_byte + spec.width));
                        window &= !mask;
                        bf_store_mem_window(self, bus, start_addr, window, bytes_len);
                        12
                    }
                    0xD => {
                        // BFFFO
                        let (pos, z) = bf_find_first_one(field, spec.width, spec.offset);
                        self.set_d(spec.reg, pos);
                        self.set_bitfield_flags(field, spec.width);
                        if z {
                            self.not_z_flag = 0;
                        }
                        12
                    }
                    0xE => {
                        // BFSET
                        self.set_bitfield_flags(field, spec.width);
                        let mask =
                            (bf_mask(spec.width) as u64) << (40 - (bit_in_byte + spec.width));
                        window |= mask;
                        bf_store_mem_window(self, bus, start_addr, window, bytes_len);
                        12
                    }
                    0xF => {
                        // BFINS
                        let src = self.d(spec.reg) & bf_mask(spec.width);
                        self.set_bitfield_flags(src, spec.width);
                        let shift = 40 - (bit_in_byte + spec.width);
                        let mask = (bf_mask(spec.width) as u64) << shift;
                        window = (window & !mask) | ((src as u64) << shift);
                        bf_store_mem_window(self, bus, start_addr, window, bytes_len);
                        12
                    }
                    _ => self.take_exception(bus, 4),
                }
            }
        }
    }

    fn decode_bitfield_spec<B: AddressBus>(&mut self, _bus: &mut B, ext: u16) -> BitFieldSpec {
        let reg = ((ext >> 12) & 7) as usize;

        let offset = if (ext & 0x0800) != 0 {
            let r = ((ext >> 6) & 7) as usize;
            self.d(r)
        } else {
            ((ext >> 6) as u32) & 31
        };

        let mut width = if (ext & 0x0020) != 0 {
            let r = (ext & 7) as usize;
            self.d(r) & 31
        } else {
            (ext as u32) & 31
        };
        if width == 0 {
            width = 32;
        }

        BitFieldSpec { reg, offset, width }
    }

    fn set_bitfield_flags(&mut self, field: u32, width: u32) {
        // N is MSB of extracted field; Z reflects field==0; V/C cleared; X unaffected.
        self.n_flag = if width == 0 {
            0
        } else if (field & (1u32 << (width - 1))) != 0 {
            0x80
        } else {
            0
        };
        self.not_z_flag = field;
        self.v_flag = 0;
        self.c_flag = 0;
    }
}

fn bf_mask(width: u32) -> u32 {
    if width >= 32 {
        0xFFFF_FFFF
    } else {
        (1u32 << width) - 1
    }
}

fn bf_sign_extend(field: u32, width: u32) -> u32 {
    if width >= 32 {
        field
    } else {
        let sign = 1u32 << (width - 1);
        if (field & sign) != 0 {
            field | (!0u32 << width)
        } else {
            field
        }
    }
}

fn bf_find_first_one(field: u32, width: u32, base_offset: u32) -> (u32, bool) {
    if field == 0 {
        // Undefined by spec; Musashi-style deterministic choice: offset + width.
        return (base_offset.wrapping_add(width), true);
    }

    for i in 0..width {
        let bit = (field >> (width - 1 - i)) & 1;
        if bit != 0 {
            return (base_offset.wrapping_add(i), false);
        }
    }
    (base_offset.wrapping_add(width), true)
}

fn bf_extract_reg_msb0(value: u32, offset: u32, width: u32) -> u32 {
    let mut out = 0u32;
    for i in 0..width {
        let pos = (offset + i) & 31;
        let bit = (value >> (31 - pos)) & 1;
        out = (out << 1) | bit;
    }
    out
}

fn bf_insert_reg_msb0(orig: u32, offset: u32, width: u32, field: u32) -> u32 {
    let mut v = orig;
    for i in 0..width {
        let pos = (offset + i) & 31;
        let bit = (field >> (width - 1 - i)) & 1;
        let shift = 31 - pos;
        v = (v & !(1u32 << shift)) | (bit << shift);
    }
    v
}

fn bf_extract_mem_window_msb0<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    start_addr: u32,
    bit_in_byte: u32,
    width: u32,
) -> (u32, u64, usize) {
    let bytes_len = (bit_in_byte + width).div_ceil(8) as usize;
    // The operand access covers exactly the bytes the field spans and no
    // others. A 68020 transfers an operand at the size it needs - byte,
    // word, three-byte or long (MC68020UM 5.3.1) - so a field within four
    // bytes is one operand cycle (8.2.14) without the processor ever
    // driving a byte outside the field. Widening the transfer to a long
    // would read and write up to three neighbouring bytes, which is
    // observable on memory-mapped registers and moves the fault boundary
    // past the end of a mapped region.
    //
    // The real-A1200 bfprobe column (Copperline timing-test/bfprobe.asm)
    // measures spans of one, two, three and four bytes at exactly the same
    // cost, with only a five-byte span adding an access. That confirms the
    // single operand cycle, but it cannot pin the transfer width: the
    // A1200's chip RAM is 32 bits wide, so every span up to four bytes is
    // one bus cycle whatever size the processor asks for. The spanned
    // width is therefore the model to hold, being the one that touches
    // only what the instruction selects.
    //
    // Each span is one access, three bytes included: read_24 goes through
    // the AddressBus three-byte hook, so a host that bills bus cycles
    // charges the single operand cycle the hardware measures rather than a
    // word plus a byte.
    //
    // The window is 40-bit big-endian aligned (byte 0 of the span in bits
    // 39..32) so the callers' fixed shift arithmetic holds for every span,
    // and bf_store_mem_window writes back exactly the same bytes.
    let mut window = match bytes_len {
        1 => (cpu.read_8(bus, start_addr) as u64) << 32,
        2 => (cpu.read_16(bus, start_addr) as u64) << 24,
        3 => (cpu.read_24(bus, start_addr) as u64) << 16,
        _ => (cpu.read_32(bus, start_addr) as u64) << 8,
    };
    if bytes_len == 5 {
        window |= cpu.read_8(bus, start_addr.wrapping_add(4)) as u64;
    }
    let shift = 40 - (bit_in_byte + width);
    let field = ((window >> shift) as u32) & bf_mask(width);
    (field, window, bytes_len)
}

fn bf_store_mem_window<B: AddressBus>(
    cpu: &mut CpuCore,
    bus: &mut B,
    start_addr: u32,
    window: u64,
    bytes_len: usize,
) {
    // Mirror the extract exactly, so the read-modify-write drives back
    // only the bytes the field spans and never a neighbour.
    match bytes_len {
        1 => cpu.write_8(bus, start_addr, (window >> 32) as u8),
        2 => cpu.write_16(bus, start_addr, (window >> 24) as u16),
        3 => cpu.write_24(bus, start_addr, (window >> 16) as u32),
        _ => cpu.write_32(bus, start_addr, (window >> 8) as u32),
    }
    if bytes_len == 5 {
        cpu.write_8(bus, start_addr.wrapping_add(4), (window & 0xFF) as u8);
    }
}
