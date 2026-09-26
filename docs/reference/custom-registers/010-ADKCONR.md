# ADKCONR
Offset: $010
Access: read
Chipset: OCS/ECS/AGA

Reads the disk, serial-break, and audio-modulation control latches.

## Bitfields

- Bits 14-13: PRECOMP1-0, disk write precompensation (not emulated).
- Bit 12: MFMPREC, MFM rather than GCR precompensation (not emulated).
- Bit 11: UARTBRK, hold serial transmit low.
- Bit 10: WORDSYNC, wait for DSKSYNC before disk read DMA.
- Bit 9: MSBSYNC, disk byte-framing control.
- Bit 8: FAST, 2 us MFM bit cells rather than 4 us GCR cells (not emulated).
- Bits 7-4: USE3PN, USE2P3, USE1P2, USE0P1; audio channel n modulates the period of channel n+1.
- Bits 3-0: USE3VN, USE2V3, USE1V2, USE0V1; audio channel n modulates the volume of channel n+1.
