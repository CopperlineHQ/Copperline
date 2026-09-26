# DENISEID
Offset: $07C
Access: read
Chipset: ECS/AGA

Identifies ECS Denise or AGA Lisa.

## Bitfields

- ECS Denise reads $FFFC.
- AGA Lisa reads $00F8.
- OCS Denise has no register here. Copperline returns $FFFF rather than the undriven bus value, so detection code reliably identifies OCS.
