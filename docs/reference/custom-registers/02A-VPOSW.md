# VPOSW
Offset: $02A
Access: write
Chipset: OCS/ECS/AGA

Writes the long-field flag and the high bit of the vertical beam counter.

## Bitfields

- Bit 15: LOF.
- Bit 0: Vertical position bit 8.

Copperline keeps the current low vertical byte, ignores the ECS/AGA vertical bits 10-9 (bits 2-1), and clamps the result to the current field length.
