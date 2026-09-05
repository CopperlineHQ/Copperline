# FMODE
Offset: $1FC
Access: write
Chipset: AGA

Selects AGA DMA fetch widths and scan doubling.

## Bitfields

- Bits 1-0: Bitplane fetch width; 0 = 16 bits, 1 or 2 = 32 bits, 3 = 64 bits.
- Bits 3-2: Sprite fetch width, using the same encoding.
- Bit 14: BSCAN2, bitplane scan doubling.
- Bit 15: SSCAN2, sprite scan doubling.
- Bits 13-4: Ignored.
