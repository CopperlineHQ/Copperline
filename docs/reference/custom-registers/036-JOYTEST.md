# JOYTEST
Offset: $036
Access: write
Chipset: OCS/ECS/AGA

Loads bits 7-2 of all four controller-port quadrature counters for testing.

## Bitfields

- Bits 15-10: Y counter bits 7-2, both ports.
- Bits 9-8: not written (Y counter bits 1-0 follow the quadrature inputs).
- Bits 7-2: X counter bits 7-2, both ports.
- Bits 1-0: not written (X counter bits 1-0 follow the quadrature inputs).

Copperline loads both ports, regardless of the attached device. The loaded bits read back in JOY0DAT and JOY1DAT behind a mouse and behind a switch joystick alike.
