# HBSTOP
Offset: $1C6
Access: write
Chipset: ECS/AGA

Sets the programmable horizontal blanking stop.

## Bitfields

- Bits 8-0: Horizontal position in colour clocks.
- Bits 15-9: Ignored.

With BEAMCON0.BLANKEN set, Copperline blanks each line from HBSTRT to HBSTOP; equal values disable programmable horizontal blanking.
