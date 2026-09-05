# BLTCON1
Offset: $042
Access: write
Chipset: OCS/ECS/AGA

Selects area or line mode, source B shift, fill, and direction.

## Bitfields

- Bits 15-12: BSH, source B shift or line-texture position.
- Bit 7: DOFF, suppress destination writes (ECS/AGA).
- Bit 6: SIGN, line error-term sign.
- Bits 4-2: EFE/IFE/FCI in area mode; SUD/SUL/AUL direction controls in line mode.
- Bit 1: DESC in area mode; SING in line mode.
- Bit 0: LINE, select line mode.

Bits 11-8 and bit 5 are unused. Fill applies to descending area blits.
