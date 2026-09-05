# BLTAMOD
Offset: $064
Access: write
Chipset: OCS/ECS/AGA

Sets the signed row-end address adjustment for blitter channel A.

## Bitfields

- Bits 15-1: Signed byte displacement; bit 0 is ignored for word alignment.

Area mode adds the modulo after each row, or subtracts it in descending mode. Line mode uses the blitter modulos for its address/error updates.
