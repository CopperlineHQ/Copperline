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

With SSCAN2 set, bit 7 of each SPRxPOS stops being a horizontal position bit and becomes that sprite's scan-double enable: a sprite with it set fetches data only on lines of its vertical start's parity and shows the held words on the lines between. The horizontal comparator ignores the bit, so positions $100-$1FF alias $000-$0FF.

With BSCAN2 set, the bitplane modulos alternate by line: BPL1MOD is added on lines of the vertical display start's parity and BPL2MOD on the others, for all planes, so a line can be fetched twice.
