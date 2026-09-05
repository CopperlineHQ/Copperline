# POT1DAT
Offset: $014
Access: read
Chipset: OCS/ECS/AGA

Reads controller port 2's analogue measurement counters.

## Bitfields

- Bits 15-8: Y counter.
- Bits 7-0: X counter.

POTGO.START begins a measurement. Each input counter advances on horizontal sync until its pin reaches the charge threshold.
