# COLOR19
Offset: $1A6
Access: write
Chipset: OCS/ECS/AGA

Sets colour register 19; AGA uses BPLCON3 banking and nibble selection.

## Bitfields

- Bit 15: Genlock transparency bit (T).
- Bits 11-8: Red nibble.
- Bits 7-4: Green nibble.
- Bits 3-0: Blue nibble.

On AGA, `BPLCON3.BANK` selects the 32-entry palette bank and LOCT selects the
high or low component nibbles. Setting `BPLCON2.RDRAM` makes this address a
palette read port; writes are ignored until RDRAM is cleared.
