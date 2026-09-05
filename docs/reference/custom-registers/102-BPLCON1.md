# BPLCON1
Offset: $102
Access: write
Chipset: OCS/ECS/AGA

Sets horizontal scroll for odd and even bitplane groups.

## Bitfields

- Bits 3-0: PF1 scroll, odd bitplanes.
- Bits 7-4: PF2 scroll, even bitplanes.
- AGA bits 11-10 and 15-14: Upper PF1/PF2 delay bits.
- AGA bits 9-8 and 13-12: Fine PF1/PF2 delay bits in superhires-pixel units.
