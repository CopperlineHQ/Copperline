# AUD3DAT
Offset: $0DA
Access: write
Chipset: OCS/ECS/AGA

Holds the next two output samples for audio channel 3.

## Bitfields

- Bits 15-8: First signed sample byte.
- Bits 7-0: Second signed sample byte.

Audio DMA delivers its words through this register. With the channel's
DMA disabled, the CPU can feed it directly: a write to an idle channel
starts output and raises the channel's audio interrupt to request the
next word.
