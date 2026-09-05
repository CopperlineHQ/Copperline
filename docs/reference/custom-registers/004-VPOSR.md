# VPOSR
Offset: $004
Access: read
Chipset: OCS/ECS/AGA

Reads the vertical beam high bit, field flags, and Agnus identification.

## Bitfields

- Bit 15: LOF, long-field flag.
- Bits 14-8: Chipset identification.
- Bit 7: LOL, long-line flag.
- Bit 0: Vertical position bit 8; VHPOSR supplies bits 7-0.
