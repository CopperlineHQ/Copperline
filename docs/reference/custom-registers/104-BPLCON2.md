# BPLCON2
Offset: $104
Access: write
Chipset: OCS/ECS/AGA

Sets playfield/sprite priorities and extended colour controls.

## Bitfields

- Bits 2-0: PF1 priority relative to sprite pairs.
- Bits 5-3: PF2 priority relative to sprite pairs.
- Bit 6: PF2PRI, place playfield 2 above playfield 1.
- Bit 7: SOGEN, genlock control.
- Bit 8: RDRAM, AGA palette readback.
- Bit 9: KILLEHB, disable extra-half-brite decoding.
- Bits 10-11: ZDCTEN/ZDBPEN, genlock transparency enables.
- Bits 14-12: ZDBPSEL, genlock bitplane selection.

Extended controls depend on the fitted Denise/Lisa revision; bit 15 is unused.
