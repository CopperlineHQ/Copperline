# BEAMCON0
Offset: $1DC
Access: write
Chipset: ECS/AGA

Selects fixed or programmable beam timing and sync/blanking controls.

## Bitfields

- Bit 14: HARDDIS, disable the hardwired display limits.
- Bit 13: LPENDIS, ignore the light-pen input.
- Bit 12: VARVBEN, use VBSTRT/VBSTOP vertical blanking.
- Bit 11: LOLDIS, disable NTSC long/short line alternation.
- Bit 10: CSCBEN, composite-sync redirection.
- Bit 9: VARVSYEN, use VSSTRT/VSSTOP vertical sync.
- Bit 8: VARHSYEN, use HSSTRT/HSSTOP horizontal sync.
- Bit 7: VARBEAMEN, use the HTOTAL/VTOTAL beam totals.
- Bit 6: DUAL, UHRES dual-display mode (not emulated).
- Bit 5: PAL, select PAL rather than NTSC timing.
- Bit 4: VARCSYEN, programmable composite-sync output enable.
- Bit 3: BLANKEN, composite blanking; Copperline applies HBSTRT/HBSTOP while it is set.
- Bit 2: CSYTRUE, composite-sync pin polarity.
- Bit 1: VSYTRUE, vertical-sync pin polarity.
- Bit 0: HSYTRUE, horizontal-sync pin polarity.

Copperline interprets PAL, VARBEAMEN, VARVBEN, BLANKEN, LOLDIS, HARDDIS, and
LPENDIS. With VARBEAMEN set, VARHSYEN and VARVSYEN enable the sync windows
that display presentation and beam traces use. CSCBEN, VARCSYEN, and the
sync-pin polarities are only latched; physical sync and genlock output is
not emulated.
