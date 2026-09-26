# BLTSIZE
Offset: $058
Access: write
Chipset: OCS/ECS/AGA

Starts an OCS-compatible blit and supplies its height and width.

## Bitfields

- Bits 15-6: Height in rows; zero means 1024.
- Bits 5-0: Width in 16-bit words; zero means 64.
