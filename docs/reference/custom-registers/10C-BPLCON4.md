# BPLCON4
Offset: $10C
Access: write
Chipset: AGA

Applies the AGA bitplane colour XOR mask and sprite palette bases.

## Bitfields

- Bits 15-8: BPLAM, XOR mask applied to the selected playfield colour address after the PF2 offset. Playfield transparency and sprite priority use the original plane bits.
- Bits 7-4: ESPRM, palette-base nibble for even sprites.
- Bits 3-0: OSPRM, palette-base nibble for odd sprites.

Attached sprite pairs take their palette base from OSPRM.
