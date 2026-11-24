# SPR1POS
Offset: $148
Access: write
Chipset: OCS/ECS/AGA

Sets sprite 1's vertical start and horizontal position.

## Bitfields

- Bits 15-8: Vertical start bits 7-0.
- Bits 7-0: Horizontal position bits 8-1.

SPRxCTL supplies the extra position bits.

On AGA, bit 7 is the top bit (SH10) of Lisa's horizontal comparator. While FMODE.SSCAN2 is set the comparator ignores it, and it enables scan doubling for this sprite instead: data is fetched on every other line, starting with the vertical start line, and each fetched line is shown twice.
