# VTOTAL
Offset: $1C8
Access: write
Chipset: ECS/AGA

Sets the last vertical line number of a programmable field.

## Bitfields

- Bits 10-0: Vertical line number.
- Bits 15-11: Ignored.

With BEAMCON0.VARBEAMEN set, each field is VTOTAL + 1 lines long.
