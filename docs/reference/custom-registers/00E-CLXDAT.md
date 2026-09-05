# CLXDAT
Offset: $00E
Access: read
Chipset: OCS/ECS/AGA

Reads and clears the accumulated sprite/playfield collision flags.

## Bitfields

- Bit 15: Reads as 1 in Copperline.
- Bits 14-9: Collisions between sprite pairs.
- Bits 8-1: Sprite-pair collisions with either playfield.
- Bit 0: Playfield 1/playfield 2 collision.

CLXCON and CLXCON2 select participating planes and match values. Reading clears bits 14-0.
