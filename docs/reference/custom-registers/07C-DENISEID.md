# DENISEID
Offset: $07C
Access: read
Chipset: ECS/AGA

Identifies ECS Denise or AGA Lisa.

## Bitfields

- Bits 15-8: Driven by ECS Denise ($FF); Lisa leaves them to the motherboard.
- Bits 7-0: Chip revision: $FC for ECS Denise, $F8 for Lisa.

ECS Denise reads $FFFC. Lisa reads $00F8 on the A1200 and CD32 and $FCF8 on the A4000, whose board supplies the high byte.

OCS Denise has no register here. Copperline returns $FFFF rather than the undriven bus value, so detection code reliably identifies OCS.
