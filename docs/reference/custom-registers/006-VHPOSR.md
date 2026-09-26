# VHPOSR
Offset: $006
Access: read
Chipset: OCS/ECS/AGA

Reads the beam position, or the latched light-pen position when enabled.

## Bitfields

- Bits 15-8: Vertical position bits 7-0.
- Bits 7-0: Horizontal position in colour clocks.

The live read runs a few colour clocks ahead of the internal beam counter, and VPOSR supplies the vertical high bit. While BPLCON0.LPEN holds a light-pen position, both bytes return that position; while BPLCON0.ERSY is set, they hold the position latched when ERSY was set.
