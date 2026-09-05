# HHPOSR
Offset: $1DA
Access: read
Chipset: ECS/AGA

Reads the UHRES horizontal-position latch.

## Bitfields

- Bits 8-0: Horizontal-position value.

Copperline stores HHPOSW and returns it through HHPOSR; the UHRES counter does not advance.
