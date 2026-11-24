# BPLCON3
Offset: $106
Access: write
Chipset: ECS/AGA

Controls ECS/AGA borders and blanking, and AGA sprite resolution and palette addressing.

## Bitfields

- Bits 15-13: BANK, palette bank (0-7) (AGA).
- Bits 12-10: PF2OF, playfield 2 palette offset (AGA).
- Bit 9: LOCT, low colour-nibble selection (AGA).
- Bits 7-6: SPRES, sprite resolution (default/lores/hires/superhires) (AGA).
- Bit 5: BRDRBLNK, blank border.
- Bit 4: BRDNTRAN, opaque border for genlock.
- Bit 2: ZDCLKEN, genlock clock output.
- Bit 1: BRDSPRT, sprites in the border (AGA).
- Bit 0: EXTBLKEN, programmable instead of fixed blanking in the picture.

Bits 8 and 3 are unused. ECS Denise and AGA Lisa latch every write; OCS Denise has no BPLCON3. BRDRBLNK, BRDNTRAN, ZDCLKEN, BRDSPRT and EXTBLKEN take effect only while BPLCON0.ECSENA is set, so they can be written first and enabled later.

ECS Denise implements bits 5, 4, 2 and 0 only: its sprite resolution follows BPLCON0 and its playfield 2 palette offset is fixed at 8, whatever is written here.

With EXTBLKEN in effect, Lisa blanks between its own HBSTRT and HBSTOP positions. ECS Denise has no blank comparators: it takes blanking from Agnus's composite-sync pin, which carries HBSTRT to HBSTOP when BEAMCON0.BLANKEN is set and the sync pulse otherwise.
