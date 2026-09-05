# HSSTOP
Offset: $1C2
Access: write
Chipset: ECS/AGA

Sets the programmable horizontal sync stop.

## Bitfields

- Bits 8-0: Horizontal position in colour clocks.
- Bits 15-9: Ignored.

With BEAMCON0 VARBEAMEN and VARHSYEN set, Copperline uses the HSSTRT/HSSTOP
window for display presentation and horizontal-sync trace events. The
window must satisfy HSSTRT < HSSTOP <= HTOTAL. These latches do not set
line length; HTOTAL does that.
