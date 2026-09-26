# DSKBYTR
Offset: $01A
Access: read
Chipset: OCS/ECS/AGA

Reads the latest disk byte and the disk DMA/sync flags.

## Bitfields

- Bit 15: DSKBYT, a new byte is available; reading clears this flag.
- Bit 14: DMAON, disk DMA is active (armed through DSKLEN and enabled in DMACON).
- Bit 13: DISKWRITE, DSKLEN selects a disk write.
- Bit 12: WORDEQUAL, the disk shifter matches DSKSYNC.
- Bits 7-0: Latest disk byte.
