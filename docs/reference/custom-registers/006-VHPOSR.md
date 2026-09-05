# VHPOSR
Offset: $006
Access: read
Chipset: OCS/ECS/AGA

Reads the beam position, or the latched light-pen position when enabled.

## Bitfields

- Bits 15-8: Vertical position bits 7-0.
- Bits 7-0: Horizontal position in colour clocks.

The live read is pipelined relative to the internal beam counter; VPOSR supplies the vertical high bit.
