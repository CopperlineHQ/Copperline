# BPLCON0
Offset: $100
Access: write
Chipset: OCS/ECS/AGA

Selects resolution, bitplane count, HAM, dual playfields, and beam controls.

## Bitfields

- Bit 15: HIRES.
- Bits 14-12: BPU2-0, bitplane count; AGA adds BPU3 at bit 4.
- Bit 11: HAM.
- Bit 10: DPF, dual playfields.
- Bit 9: COLOR, colour output enable.
- Bit 8: GAUD, genlock audio control.
- Bit 7: UHRES (not emulated).
- Bit 6: SHRES (ECS/AGA).
- Bit 5: BYPASS (AGA).
- Bit 3: LPEN, light-pen latch enable.
- Bit 2: LACE, interlace.
- Bit 1: ERSY, external resynchronization.
- Bit 0: ECSENA, enable extended Denise controls.
