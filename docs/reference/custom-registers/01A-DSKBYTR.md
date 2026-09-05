# DSKBYTR
Offset: $01A
Access: read
Chipset: OCS/ECS/AGA

Reads the latest disk byte and the disk DMA/sync flags.

## Bitfields

- Bit 15: DSKBYT, a new byte is available; reading clears this flag.
- Bit 14: DMAON.
- Bit 13: DISKWRITE.
- Bit 12: WORDEQUAL, the disk shifter matches DSKSYNC.
- Bits 7-0: Latest disk byte.
