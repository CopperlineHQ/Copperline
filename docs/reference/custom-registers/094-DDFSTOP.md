# DDFSTOP
Offset: $094
Access: write
Chipset: OCS/ECS/AGA

Sets the horizontal bitplane-fetch stop comparison.

## Bitfields

- Bits 7-2: OCS horizontal comparison in colour clocks.
- Bit 1: Additional ECS/AGA comparison precision.
- Bit 0: Ignored.

The sequencer's state, DMA enables, and fetch width determine the actual transfers; DDFSTOP does not cut off a fetch group already in progress.
