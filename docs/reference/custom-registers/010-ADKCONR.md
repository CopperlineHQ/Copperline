# ADKCONR
Offset: $010
Access: read
Chipset: OCS/ECS/AGA

Reads the disk, serial-break, and audio-modulation control latches.

## Bitfields

- Bits 14-13: Disk write precompensation selection.
- Bit 12: MFMPREC, MFM precompensation mode.
- Bit 11: UARTBRK, hold serial transmit low.
- Bit 10: WORDSYNC, wait for DSKSYNC before disk read DMA.
- Bit 9: MSBSYNC, disk byte-framing control.
- Bit 8: FAST, disk bit-cell timing selection.
- Bits 7-4: Audio period-modulation enables for channels 3-0.
- Bits 3-0: Audio volume-modulation enables for channels 3-0.
