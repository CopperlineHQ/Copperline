# BEAMCON0
Offset: $1DC
Access: write
Chipset: ECS/AGA

Selects fixed or programmable beam timing and sync/blanking controls.

## Bitfields

- Bit 14: HARDDIS, disable the hardwired display limits.
- Bit 13: LPENDIS, VPOSR and VHPOSR read the running beam counters instead of the light-pen latch.
- Bit 12: VARVBEN, use VBSTRT/VBSTOP vertical blanking.
- Bit 11: LOLDIS, disable the long/short line alternation that runs while PAL is clear, on a programmable scan (HTOTAL + 1 and HTOTAL + 2 colour clocks) as on the fixed NTSC one (227 and 228).
- Bit 10: CSCBEN, composite-sync redirection.
- Bit 9: VARVSYEN, use VSSTRT/VSSTOP vertical sync.
- Bit 8: VARHSYEN, use HSSTRT/HSSTOP horizontal sync.
- Bit 7: VARBEAMEN, use the HTOTAL/VTOTAL beam totals.
- Bit 6: DUAL, UHRES dual-display mode (not emulated).
- Bit 5: PAL, select PAL rather than NTSC timing.
- Bit 4: VARCSYEN, the CSY* pin carries the programmed sync pulse (HSSTRT/HSSTOP) instead of the hardwired one.
- Bit 3: BLANKEN, the CSY* pin carries composite blank (HBSTRT/HBSTOP) instead of composite sync.
- Bit 2: CSYTRUE, composite-sync pin polarity.
- Bit 1: VSYTRUE, vertical-sync pin polarity.
- Bit 0: HSYTRUE, horizontal-sync pin polarity.

Copperline interprets PAL, VARBEAMEN, VARVBEN, BLANKEN, VARCSYEN, LOLDIS,
HARDDIS, and LPENDIS. With VARBEAMEN set, VARHSYEN and VARVSYEN enable the
sync windows that display presentation and beam traces use.

BLANKEN and VARCSYEN choose what the CSY* pin carries, and an ECS Denise
takes its horizontal blanking from that pin once BPLCON0.ECSENA and
BPLCON3.EXTBLKEN are set: HBSTRT/HBSTOP under BLANKEN, the programmed sync
pulse under VARCSYEN alone, and their overlap with both. Neither bit blanks
the picture by itself, and an OCS Denise always keeps its fixed blank. Lisa
compares its own HBSTRT/HBSTOP and ignores the pin. The pin's vertical
component is not modelled. CSCBEN and the sync-pin polarities are only
latched; physical sync and genlock output is not emulated.
