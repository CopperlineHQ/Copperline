# DMACONR
Offset: $002
Access: read
Chipset: OCS/ECS/AGA

Reports DMA enable state and blitter status.

## Bitfields

- Bit 14: BBUSY, blitter busy.
- Bit 13: BZERO, all destination results in the current or last blit are zero.
- Bit 10: BLTPRI, blitter priority over CPU chip-bus requests.
- Bit 9: DMAEN, master DMA enable.
- Bits 8-4: BPLEN, COPEN, BLTEN, SPREN, DSKEN.
- Bits 3-0: AUD3EN through AUD0EN.
