# JOY1DAT
Offset: $00C
Access: read
Chipset: OCS/ECS/AGA

Reads controller port 2's quadrature counters or digital joystick directions.

## Bitfields

- Bits 15-8: Y counter.
- Bits 7-0: X counter.

Joystick direction switches feed the quadrature inputs: they set bits 1-0 of each counter, while bits 7-2 keep the count the chip holds (JOYTEST loads them). Fire buttons are read through CIA-A and POTGOR, not this word.
