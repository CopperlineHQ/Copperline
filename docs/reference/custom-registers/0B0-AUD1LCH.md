# AUD1LCH
Offset: $0B0
Access: write
Chipset: OCS/ECS/AGA

Sets the upper word of audio channel 1's sample pointer.

## Bitfields

- Bits 4-0: Chip-RAM address bits 20-16.
- Bits 15-5: Ignored.

The usable address range also depends on the fitted Agnus and chip RAM.

