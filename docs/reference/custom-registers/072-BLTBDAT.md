# BLTBDAT
Offset: $072
Access: write
Chipset: OCS/ECS/AGA

Holds the source B word used by the blitter.

## Bitfields

- Bits 15-0: Source data.

A write immediately runs the B barrel shifter using the current BLTCON1
BSH and DESC values. Area blits with USEB clear consume this shifted hold
word; changing BSH before starting the blit does not shift it again.
Line mode uses BLTBDAT as its texture and applies the live BSH value.
