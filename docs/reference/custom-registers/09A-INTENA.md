# INTENA
Offset: $09A
Access: write
Chipset: OCS/ECS/AGA

Sets or clears interrupt-source enables and the master enable.

## Bitfields

- Bit 15: SET/CLR; 1 sets selected bits, 0 clears them.
- Bit 14: INTEN, master interrupt enable.
- Bit 13: EXTER.
- Bit 12: DSKSYNC.
- Bit 11: RBF.
- Bits 10-7: AUD3 through AUD0.
- Bit 6: BLIT.
- Bit 5: VERTB.
- Bit 4: COPER.
- Bit 3: PORTS.
- Bit 2: SOFT.
- Bit 1: DSKBLK.
- Bit 0: TBE.
