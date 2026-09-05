# BPL7DAT
Offset: $11C
Access: write
Chipset: AGA

Holds display data for bitplane 7.

## Bitfields

- Bits 15-0: Planar pixel data, most-significant bit first.

A BPL1DAT write strobes the bitplane output load; writes to the other BPLxDAT latches do not trigger that load by themselves.
