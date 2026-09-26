# AUD3PER
Offset: $0D6
Access: write
Chipset: OCS/ECS/AGA

Sets audio channel 3's sample playback period.

## Bitfields

- Bits 15-0: Period in colour clocks per 8-bit sample.

DMA supplies two samples per word; a shorter period raises the playback rate.
