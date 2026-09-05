# BPL7PTH
Offset: $0F8
Access: write
Chipset: AGA

Sets the upper word of bitplane 7's DMA pointer.

## Bitfields

- Bits 4-0: Chip-RAM address bits 20-16.
- Bits 15-5: Ignored.

The usable address range also depends on the fitted Agnus and chip RAM.

