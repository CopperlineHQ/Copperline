# SPR7CTL
Offset: $17A
Access: write
Chipset: OCS/ECS/AGA

Sets sprite 7's vertical stop, attachment, and extra position bits.

## Bitfields

Copperline decodes these fields:

- Bits 15-8: Vertical stop bits 7-0.
- Bit 7: Attach this sprite pair for 16-colour output (set on the odd sprite).
- Bit 6: SV9, vertical start bit 9 (ECS Agnus and Alice; ignored on OCS).
- Bit 5: EV9, vertical stop bit 9 (ECS Agnus and Alice; ignored on OCS).
- Bit 4: Horizontal position, 70 ns: applied in every mode on AGA, and when
  BPLCON0.SHRES is set on ECS.
- Bit 3: Horizontal position, 35 ns (AGA).
- Bit 2: Vertical start bit 8.
- Bit 1: Vertical stop bit 8.
- Bit 0: Horizontal position bit 0.

Bits 6 and 5 let a sprite sit below line 511 of a programmable scan (BEAMCON0.VARBEAMEN).

A control write disarms the sprite until new data arms it.
