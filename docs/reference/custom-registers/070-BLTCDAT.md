# BLTCDAT
Offset: $070
Access: write
Chipset: OCS/ECS/AGA

Holds the source C word used by the blitter.

## Bitfields

- Bits 15-0: Source data.

When the channel's DMA is disabled, software can supply its data through this latch.
