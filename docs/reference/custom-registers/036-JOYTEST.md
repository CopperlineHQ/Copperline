# JOYTEST
Offset: $036
Access: write
Chipset: OCS/ECS/AGA

Loads both controller ports' quadrature counters for testing.

## Bitfields

- Bits 15-8: Y counter value.
- Bits 7-0: X counter value.

Copperline loads both bytes into both ports, regardless of the attached device.
