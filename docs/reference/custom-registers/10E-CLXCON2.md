# CLXCON2
Offset: $10E
Access: write
Chipset: AGA

Extends AGA collision matching to bitplanes 7 and 8.

## Bitfields

- Bits 7-6: Enable bitplanes 8 and 7.
- Bits 1-0: Required match values for bitplanes 8 and 7.

CLXCON writes reset this latch.
