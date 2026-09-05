# SPR0DATB
Offset: $146
Access: write
Chipset: OCS/ECS/AGA

Holds sprite 0's high pixel bitplane.

## Bitfields

- Bits 15-0: Sixteen planar pixels, most-significant bit first.

Matching DATA/DATB bits form a two-bit pixel; zero is transparent. A DATA write arms the sprite, while DATB updates its other plane.
