# DSKLEN
Offset: $024
Access: write
Chipset: OCS/ECS/AGA

Arms disk DMA, selects transfer direction, and sets the word count.

## Bitfields

- Bit 15: DMAEN. Write the enabled value twice to start DMA; clearing this bit disarms it.
- Bit 14: WRITE, 1 for disk writes and 0 for reads.
- Bits 13-0: Transfer length in 16-bit words.

DMACON must also enable master and disk DMA. ADKCON.WORDSYNC can defer reads until DSKSYNC matches.
