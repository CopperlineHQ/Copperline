# BLTCON0
Offset: $040
Access: write
Chipset: OCS/ECS/AGA

Selects source A shift, DMA channels, and the blitter minterm.

## Bitfields

- Bits 15-12: ASH, source A shift (0-15).
- Bits 11-8: USEA, USEB, USEC, USED, respectively.
- Bits 7-0: LF, the eight-entry truth table for the A/B/C Boolean operation.
