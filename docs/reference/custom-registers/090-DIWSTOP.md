# DIWSTOP
Offset: $090
Access: write
Chipset: OCS/ECS/AGA

Sets the display-window stop position.

## Bitfields

- Bits 15-8: Vertical position low byte.
- Bits 7-0: Horizontal position low byte.

OCS makes vertical bit 8 the complement of bit 7 and fixes horizontal bit 8 at one. ECS/AGA can supply the high bits through DIWHIGH, which must be written after DIWSTRT/DIWSTOP.
