# SERPER
Offset: $032
Access: write
Chipset: OCS/ECS/AGA

Sets serial bit timing and word length.

## Bitfields

- Bit 15: LONG, select 9 data bits instead of 8.
- Bits 14-0: Bit period in colour clocks, minus one.

The bit rate is the colour-clock frequency divided by this field plus one.
