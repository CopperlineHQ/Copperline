# AUD0VOL
Offset: $0A8
Access: write
Chipset: OCS/ECS/AGA

Sets audio channel 0's volume latch.

## Bitfields

- Bits 5-0: Volume from 0 to 63.
- Bit 6: Select maximum volume (64), regardless of bits 5-0.
- Bits 15-7: Ignored.

The live volume reloads at the next output-word boundary.
