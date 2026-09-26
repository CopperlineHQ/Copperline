# FMODE
Offset: $1FC
Access: write
Chipset: AGA

Selects AGA DMA fetch widths and scan doubling.

## Bitfields

- Bit 15: SSCAN2, sprite scan doubling.
- Bit 14: BSCAN2, bitplane scan doubling.
- Bits 13-4: Ignored.
- Bits 3-2: SPAGEM and SPR32, sprite fetch width; 0 = 16 bits, 1 or 2 = 32 bits, 3 = 64 bits.
- Bits 1-0: BPAGEM and BPL32, bitplane fetch width, with the same encoding.
