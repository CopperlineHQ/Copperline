# AUD2LEN
Offset: $0C4
Access: write
Chipset: OCS/ECS/AGA

Sets audio channel 2's DMA block length.

## Bitfields

- Bits 15-0: Length in 16-bit words, two 8-bit samples per word. Zero represents 65536 words.
