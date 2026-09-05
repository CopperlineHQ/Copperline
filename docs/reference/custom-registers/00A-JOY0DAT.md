# JOY0DAT
Offset: $00A
Access: read
Chipset: OCS/ECS/AGA

Reads controller port 1's quadrature counters or digital joystick directions.

## Bitfields

- Bits 15-8: Y counter.
- Bits 7-0: X counter.

Joystick direction switches feed the quadrature inputs. Fire buttons are read through CIA-A and POTGOR, not this word.
