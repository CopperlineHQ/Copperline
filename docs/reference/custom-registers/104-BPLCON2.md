# BPLCON2
Offset: $104
Access: write
Chipset: OCS/ECS/AGA

Sets playfield/sprite priorities and extended colour controls.

## Bitfields

- Bits 14-12: ZDBPSEL, genlock bitplane selection.
- Bit 11: ZDBPEN, genlock bitplane-transparency enable.
- Bit 10: ZDCTEN, genlock colour-transparency enable.
- Bit 9: KILLEHB, disable extra-half-brite decoding.
- Bit 8: RDRAM, make the COLORxx addresses read the palette (AGA).
- Bit 7: SOGEN, genlock control (not emulated).
- Bit 6: PF2PRI, place playfield 2 above playfield 1.
- Bits 5-3: PF2 priority relative to sprite pairs.
- Bits 2-0: PF1 priority relative to sprite pairs.

Extended controls depend on the fitted Denise/Lisa revision; bit 15 is unused.
