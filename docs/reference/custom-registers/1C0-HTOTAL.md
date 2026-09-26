# HTOTAL
Offset: $1C0
Access: write
Chipset: ECS/AGA

Sets the last horizontal colour-clock position of a programmable line.

## Bitfields

- Bits 8-0: Horizontal position in colour clocks.
- Bits 15-9: Ignored.

With BEAMCON0.VARBEAMEN set, each line is HTOTAL + 1 colour clocks long.
