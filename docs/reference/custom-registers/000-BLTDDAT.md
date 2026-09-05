# BLTDDAT
Offset: $000
Access: read
Chipset: OCS/ECS/AGA

Blitter destination-data read address. Copperline does not expose a destination latch through this address.

## Bitfields

No destination bits are driven by the current model; a CPU read returns the custom-bus residue.
