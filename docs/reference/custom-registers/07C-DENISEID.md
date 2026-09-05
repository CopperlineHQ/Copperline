# DENISEID
Offset: $07C
Access: read
Chipset: ECS/AGA

Identifies ECS Denise or AGA Lisa.

## Bitfields

- ECS Denise reads $FFFC.
- AGA Lisa reads $00F8.
- OCS has no register here; Copperline returns $FFFF as a detection workaround instead of the floating-bus residue.
