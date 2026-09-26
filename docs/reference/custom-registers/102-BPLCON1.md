# BPLCON1
Offset: $102
Access: write
Chipset: OCS/ECS/AGA

Sets horizontal scroll for odd and even bitplane groups.

## Bitfields

- Bits 15-14: PF2 delay bits 7-6 (AGA).
- Bits 13-12: PF2 delay bits 1-0, in superhires pixels (AGA).
- Bits 11-10: PF1 delay bits 7-6 (AGA).
- Bits 9-8: PF1 delay bits 1-0, in superhires pixels (AGA).
- Bits 7-4: PF2 scroll for the even bitplanes, in lores pixels (AGA delay bits 5-2).
- Bits 3-0: PF1 scroll for the odd bitplanes, in lores pixels (AGA delay bits 5-2).
