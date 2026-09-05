# BLTSIZV
Offset: $05C
Access: write
Chipset: ECS/AGA

Latches the extended blitter height for a later BLTSIZH start.

## Bitfields

- Bits 14-0: Height in rows; zero means 32768.
- Bit 15: Ignored.

Writing this register alone does not start a blit.
