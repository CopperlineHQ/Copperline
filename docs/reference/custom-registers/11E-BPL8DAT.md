# BPL8DAT
Offset: $11E
Access: write
Chipset: AGA

Holds display data for bitplane 8.

## Bitfields

- Bits 15-0: Planar pixel data, most-significant bit first.

A BPL1DAT write strobes the bitplane output load; writes to the other BPLxDAT latches do not trigger that load by themselves.
