# DIWSTOP
Offset: $090
Access: write
Chipset: OCS/ECS/AGA

Sets the display-window stop position.

## Bitfields

- Bits 15-8: Vertical position low byte.
- Bits 7-0: Horizontal position low byte.

OCS supplies implicit high bits. ECS/AGA can override them with DIWHIGH, which must be written after DIWSTRT/DIWSTOP.
