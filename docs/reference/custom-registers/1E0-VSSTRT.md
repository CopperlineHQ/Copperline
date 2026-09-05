# VSSTRT
Offset: $1E0
Access: write
Chipset: ECS/AGA

Sets the programmable vertical sync start.

## Bitfields

- Bits 10-0: Vertical line number.
- Bits 15-11: Ignored.

With BEAMCON0 VARBEAMEN and VARVSYEN set, Copperline uses the VSSTRT/VSSTOP
window for display presentation and vertical-sync trace state. The
window must satisfy VSSTRT < VSSTOP <= VTOTAL. These latches do not set
field length; VTOTAL does that.
