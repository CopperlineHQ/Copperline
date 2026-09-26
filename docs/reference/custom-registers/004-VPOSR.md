# VPOSR
Offset: $004
Access: read
Chipset: OCS/ECS/AGA

Reads the long-field and long-line flags, the Agnus identification, and the high bit of the vertical beam position.

## Bitfields

- Bit 15: LOF, long-field flag.
- Bits 14-8: Agnus identification.
- Bit 7: LOL, long-line flag.
- Bit 0: Vertical position bit 8; VHPOSR supplies bits 7-0.

ECS and AGA Agnus extend the vertical position to bits 2-0 (V10-V8); Copperline returns only V8, so bits 2-1 read as zero. While BPLCON0.LPEN holds a light-pen position, bit 0 comes from that position; while BPLCON0.ERSY is set, LOF, LOL, and bit 0 keep their values from when ERSY was set.
