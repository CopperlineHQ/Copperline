# BLTAFWM
Offset: $044
Access: write
Chipset: OCS/ECS/AGA

Masks source A on the first word of each blitter row.

## Bitfields

- Bits 15-0: AND mask applied before source A shifting; a cleared bit suppresses the corresponding input bit.
