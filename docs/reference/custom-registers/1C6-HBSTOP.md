# HBSTOP
Offset: $1C6
Access: write
Chipset: ECS/AGA

Sets the programmable horizontal blanking stop.

## Bitfields

- Bits 15-11: Ignored.
- Bit 10: H2, 140 ns step (AGA Lisa).
- Bit 9: H1, 70 ns step (AGA Lisa).
- Bit 8: H0, 35 ns step (AGA Lisa).
- Bits 7-0: Horizontal position in colour clocks (280 ns).

Blanking runs from HBSTRT to HBSTOP on each line; equal values disable it, and a start at or after the stop wraps through the line origin.

AGA Lisa compares all eleven bits itself and blanks the picture while BPLCON0.ECSENA and BPLCON3.EXTBLKEN are set. ECS Denise has no such comparators: Agnus compares the colour-clock position and puts the window on its composite-sync pin when BEAMCON0.BLANKEN is set, and Denise blanks from that pin while ECSENA and EXTBLKEN are set. OCS Denise always uses its fixed blanking.
