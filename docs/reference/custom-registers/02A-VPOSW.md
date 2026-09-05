# VPOSW
Offset: $02A
Access: write
Chipset: OCS/ECS/AGA

Writes the long-field flag and high bit of the vertical beam counter.

## Bitfields

- Bit 15: LOF.
- Bit 0: Vertical position bit 8.

Copperline retains the current low vertical byte and clamps the result to the configured field length.
