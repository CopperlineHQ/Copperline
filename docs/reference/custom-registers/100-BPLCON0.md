# BPLCON0
Offset: $100
Access: write
Chipset: OCS/ECS/AGA

Selects resolution, bitplane count, HAM, dual playfields, and beam controls.

## Bitfields

- Bit 15: HIRES.
- Bits 14-12: BPU2-0, bitplane count.
- Bit 11: HAM.
- Bit 10: DPF, dual playfields.
- Bit 9: COLOR, composite colour enable (not emulated).
- Bit 8: GAUD, genlock audio enable (not emulated).
- Bit 7: UHRES, ultra-high-resolution mode (ECS/AGA, not emulated).
- Bit 6: SHRES, superhires (ECS/AGA).
- Bit 5: BYPASS, bypass the colour table (AGA, not emulated).
- Bit 4: BPU3, bitplane count bit 3 (AGA).
- Bit 3: LPEN, light-pen latch enable.
- Bit 2: LACE, interlace.
- Bit 1: ERSY, external resynchronization; Copperline freezes the VPOSR/VHPOSR readback while it is set.
- Bit 0: ECSENA, enable the BPLCON3 border and genlock controls (ECS/AGA).
