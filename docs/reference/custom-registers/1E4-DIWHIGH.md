# DIWHIGH
Offset: $1E4
Access: write
Chipset: ECS/AGA

Supplies extended display-window position bits on ECS/AGA.

## Bitfields

- Bit 13: Horizontal stop bit 8.
- Bit 12: Horizontal stop H1, 70 ns step (AGA).
- Bit 11: Horizontal stop H0, 35 ns step (AGA).
- Bits 10-8: Vertical stop bits 10-8.
- Bit 5: Horizontal start bit 8.
- Bit 4: Horizontal start H1, 70 ns step (AGA).
- Bit 3: Horizontal start H0, 35 ns step (AGA).
- Bits 2-0: Vertical start bits 10-8.

Copperline decodes only these fields. Write DIWHIGH after DIWSTRT/DIWSTOP: a later write to either restores the OCS implicit high bits.

The H1 and H0 bits move a window edge by fractions of the low-resolution pixel DIWSTRT/DIWSTOP name, in every resolution; the fetched picture does not move with it. Only Lisa has them, and ECS Denise ignores the bits. Copperline shows the 35 ns step where the frame is rendered at 35 ns per column (a programmable-scan superhires frame) and drops it on the 70 ns canvas.
