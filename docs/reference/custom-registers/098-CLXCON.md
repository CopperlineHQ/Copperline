# CLXCON
Offset: $098
Access: write
Chipset: OCS/ECS/AGA

Selects sprite participation and bitplane matches for collision detection.

## Bitfields

- Bits 15-12: Include odd sprites 7, 5, 3, and 1 in their respective pairs.
- Bits 11-6: Enable bitplanes 6-1 for comparison.
- Bits 5-0: Required match values for bitplanes 6-1.

On AGA, writing CLXCON also clears CLXCON2.
