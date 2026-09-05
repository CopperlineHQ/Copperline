# BPL8PTH
Offset: $0FC
Access: write
Chipset: AGA

Sets the upper word of bitplane 8's DMA pointer.

## Bitfields

- Bits 4-0: Chip-RAM address bits 20-16.
- Bits 15-5: Ignored.

The usable address range also depends on the fitted Agnus and chip RAM.

