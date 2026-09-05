# AUD3LEN
Offset: $0D4
Access: write
Chipset: OCS/ECS/AGA

Sets audio channel 3's DMA block length.

## Bitfields

- Bits 15-0: Length in 16-bit words, two 8-bit samples per word. Zero represents 65536 words.
