# BPLCON4
Offset: $10C
Access: write
Chipset: AGA

Applies the AGA bitplane colour XOR mask and sprite palette bases.

## Bitfields

- Bits 15-8: BPLAM, XOR mask for bitplane colour indices.
- Bits 7-4: ESPRM, palette-base nibble for even sprites.
- Bits 3-0: OSPRM, palette-base nibble for odd sprites.
