# DMACON
Offset: $096
Access: write
Chipset: OCS/ECS/AGA

Sets or clears the DMA enables and blitter priority.

## Bitfields

- Bit 15: SET/CLR; 1 sets the selected bits, 0 clears them.
- Bit 10: BLTPRI, blitter priority over CPU chip-bus requests.
- Bit 9: DMAEN, master DMA enable.
- Bits 8-4: BPLEN, COPEN, BLTEN, SPREN, DSKEN.
- Bits 3-0: AUD3EN through AUD0EN.

Bits 14-13 are status in DMACONR and cannot be written here.
