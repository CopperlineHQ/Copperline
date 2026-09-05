# BPLCON3
Offset: $106
Access: write
Chipset: ECS/AGA

Controls ECS/AGA borders, sprite resolution, and AGA palette addressing.

## Bitfields

- Bits 15-13: BANK, AGA palette bank (0-7).
- Bits 12-10: PF2OF, AGA playfield 2 palette offset.
- Bit 9: LOCT, AGA low colour-nibble selection.
- Bits 7-6: SPRES, sprite resolution (default/lores/hires/superhires).
- Bit 5: BRDRBLNK, blank border.
- Bit 4: BRDNTRAN, opaque border for genlock.
- Bit 2: ZDCLKEN, genlock clock output.
- Bit 1: BRDRSPRT, sprites in the border.
- Bit 0: EXTBLKEN, external blanking control.

Bits 8 and 3 are unused. ECS implements only the extended Denise subset; AGA adds the palette and sprite controls.
