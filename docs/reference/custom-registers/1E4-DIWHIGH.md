# DIWHIGH
Offset: $1E4
Access: write
Chipset: ECS/AGA

Supplies extended display-window position bits on ECS/AGA.

## Bitfields

- Bits 2-0: Vertical start bits 10-8.
- Bit 5: Horizontal start bit 8.
- Bits 10-8: Vertical stop bits 10-8.
- Bit 13: Horizontal stop bit 8.

These are the fields Copperline decodes. Write DIWHIGH after DIWSTRT/DIWSTOP; either of those later writes restores implicit high-bit decoding.
