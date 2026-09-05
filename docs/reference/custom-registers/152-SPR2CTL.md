# SPR2CTL
Offset: $152
Access: write
Chipset: OCS/ECS/AGA

Sets sprite 2's vertical stop, attachment, and extra position bits.

## Bitfields

Copperline decodes these fields:

- Bits 15-8: Vertical stop bits 7-0.
- Bit 7: Attach this sprite pair for 16-colour output (set on the odd sprite).
- Bit 4: Horizontal subpixel position when BPLCON0.SHRES is set.
- Bit 2: Vertical start bit 8.
- Bit 1: Vertical stop bit 8.
- Bit 0: Horizontal position bit 0.

A control write disarms the sprite until new data arms it.
