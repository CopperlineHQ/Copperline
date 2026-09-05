# BPL1MOD
Offset: $108
Access: write
Chipset: OCS/ECS/AGA

Sets the row-end DMA pointer adjustment for the odd-numbered bitplanes.

## Bitfields

- Bits 15-1: Signed byte displacement added after a fetched row.
- Bit 0: Ignored for word alignment.
