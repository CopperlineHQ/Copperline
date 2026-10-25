//! Decoding of the six-bit effective-address field used by 68k opcodes.

/// An effective-address encoding after splitting the opcode's mode and
/// register fields.
///
/// Variants carrying a register number use the low three bits and therefore
/// identify registers 0 through 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    /// Data register direct (`Dn`).
    DataRegister(u8),
    /// Address register direct (`An`).
    AddressRegister(u8),
    /// Address register indirect (`(An)`).
    AddressIndirect(u8),
    /// Address register indirect with postincrement (`(An)+`).
    AddressPostIncrement(u8),
    /// Address register indirect with predecrement (`-(An)`).
    AddressPreDecrement(u8),
    /// Address register indirect with a 16-bit displacement (`(d16,An)`).
    AddressDisplacement(u8),
    /// Address register indirect with an index extension (`(d8,An,Xn)` or
    /// the 68020+ full extension format).
    AddressIndex(u8),
    /// Sign-extended 16-bit absolute address (`(xxx).W`).
    AbsoluteShort,
    /// 32-bit absolute address (`(xxx).L`).
    AbsoluteLong,
    /// Program-counter relative with a 16-bit displacement (`(d16,PC)`).
    PcDisplacement,
    /// Program-counter relative with an index extension (`(d8,PC,Xn)` or
    /// the 68020+ full extension format).
    PcIndex,
    /// Immediate data encoded in extension words.
    Immediate,
}

impl AddressingMode {
    /// Decode the opcode's three-bit `mode` and three-bit `reg` fields.
    ///
    /// Returns `None` for reserved mode-7 register values and for values
    /// outside the three-bit mode range.
    pub fn decode(mode: u8, reg: u8) -> Option<Self> {
        match mode {
            0b000 => Some(Self::DataRegister(reg)),
            0b001 => Some(Self::AddressRegister(reg)),
            0b010 => Some(Self::AddressIndirect(reg)),
            0b011 => Some(Self::AddressPostIncrement(reg)),
            0b100 => Some(Self::AddressPreDecrement(reg)),
            0b101 => Some(Self::AddressDisplacement(reg)),
            0b110 => Some(Self::AddressIndex(reg)),
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
}
