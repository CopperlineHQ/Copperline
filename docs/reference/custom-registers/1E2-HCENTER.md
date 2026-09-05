# HCENTER
Offset: $1E2
Access: write
Chipset: ECS/AGA

Latches the half-line sync position for interlaced timing.

## Bitfields

- Bits 8-0: Horizontal position in colour clocks.
- Bits 15-9: Ignored.

Copperline stores this value for debugger inspection and byte-write
reconstruction. It does not currently apply HCENTER to sync timing.
