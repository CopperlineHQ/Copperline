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
- Bit 4: VARCSYEN, programmable composite-sync output enable.
- Bit 3: BLANKEN.
- Bit 2: CSYTRUE, composite-sync pin polarity.
- Bit 1: VSYTRUE, vertical-sync pin polarity.
- Bit 0: HSYTRUE, horizontal-sync pin polarity.

Copperline models beam, blanking, and light-pen controls. With VARBEAMEN set,
VARHSYEN and VARVSYEN enable sync windows used by display presentation and
beam traces. Composite-sync routing and sync-pin polarities are latched;
physical sync/genlock output is not emulated.
