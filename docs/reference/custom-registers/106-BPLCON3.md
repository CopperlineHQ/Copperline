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
- Bit 1: BRDSPRT, sprites in the border.
- Bit 0: EXTBLKEN, external blanking control (not emulated).

Bits 8 and 3 are unused. ECS Denise latches BPLCON3 only while BPLCON0.ECSENA is set; AGA Lisa always latches it. The border and genlock controls (bits 5-0) take effect only while ECSENA is set.
