# BPL1PTH
Offset: $0E0
Access: write
Chipset: OCS/ECS/AGA

Sets the upper word of bitplane 1's DMA pointer.

## Bitfields

- Bits 4-0: Chip-RAM address bits 20-16.
- Bits 15-5: Ignored.

The usable address range also depends on the fitted Agnus and chip RAM.

