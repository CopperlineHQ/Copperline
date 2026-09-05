# BEAMCON0
Offset: $1DC
Access: write
Chipset: ECS/AGA

Selects fixed or programmable beam timing and sync/blanking controls.

## Bitfields

- Bit 14: HARDDIS.
- Bit 13: LPENDIS.
- Bit 12: VARVBEN.
- Bit 11: LOLDIS.
- Bit 10: CSCBEN.
- Bit 9: VARVSYEN.
- Bit 8: VARHSYEN.
- Bit 7: VARBEAMEN, use programmable beam totals.
- Bit 6: DUAL (UHRES dual mode is not emulated).
- Bit 5: PAL, select PAL rather than NTSC timing.
- Bit 3: BLANKEN.

Copperline models beam, blanking, and light-pen controls. External genlock/sync output is not a physical host signal.
