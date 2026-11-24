# VPOSW
Offset: $02A
Access: write
Chipset: OCS/ECS/AGA

Writes the long-field flag and the high bits of the vertical beam counter.

## Bitfields

- Bit 15: LOF.
- Bits 2-1: Vertical position bits 10-9 (ECS Agnus and Alice; ignored on OCS).
- Bit 0: Vertical position bit 8.

Any write also clears the long-line flag (LOL); bit 7 does not load it. Copperline keeps the current low vertical byte and clamps the result to the current field length.
