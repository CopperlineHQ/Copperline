# DIWSTRT
Offset: $08E
Access: write
Chipset: OCS/ECS/AGA

Sets the display-window start position.

## Bitfields

- Bits 15-8: Vertical position low byte.
- Bits 7-0: Horizontal position low byte.

OCS fixes vertical and horizontal bit 8 at zero. ECS/AGA can supply the high bits through DIWHIGH, which must be written after DIWSTRT/DIWSTOP.
