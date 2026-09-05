# HHPOSW
Offset: $1D8
Access: write
Chipset: ECS/AGA

Writes the UHRES horizontal-position latch.

## Bitfields

- Bits 8-0: Horizontal-position value.

Copperline stores HHPOSW and returns it through HHPOSR; the UHRES counter does not advance.
