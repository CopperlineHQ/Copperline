# BPL1DAT
Offset: $110
Access: write
Chipset: OCS/ECS/AGA

Holds display data for bitplane 1.

## Bitfields

- Bits 15-0: Planar pixel data, most-significant bit first.

A BPL1DAT write strobes the bitplane output load; writes to the other BPLxDAT latches do not trigger that load by themselves.
