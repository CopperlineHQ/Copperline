# DSKDAT
Offset: $026
Access: write
Chipset: OCS/ECS/AGA

Supplies a raw word to the disk write path.

## Bitfields

- Bits 15-0: Encoded disk data; AmigaDOS sector encoding is the guest software's responsibility.
