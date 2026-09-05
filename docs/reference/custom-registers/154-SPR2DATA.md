# SPR2DATA
Offset: $154
Access: write
Chipset: OCS/ECS/AGA

Holds sprite 2's low pixel bitplane.

## Bitfields

- Bits 15-0: Sixteen planar pixels, most-significant bit first.

Matching DATA/DATB bits form a two-bit pixel; zero is transparent. A DATA write arms the sprite, while DATB updates its other plane.
