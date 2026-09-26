# CLXDAT
Offset: $00E
Access: read
Chipset: OCS/ECS/AGA

Reads and clears the accumulated sprite/playfield collision flags.

## Bitfields

- Bit 15: Reads as 1 in Copperline.
- Bits 14-9: Collisions between sprite pairs.
- Bits 8-5: Sprite-pair collisions with the even bitplanes (playfield 2).
- Bits 4-1: Sprite-pair collisions with the odd bitplanes (playfield 1).
- Bit 0: Playfield 1/playfield 2 collision.

CLXCON and CLXCON2 select participating planes and match values. Reading clears bits 14-0.
