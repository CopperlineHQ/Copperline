# COPJMP2
Offset: $08A
Access: write
Chipset: OCS/ECS/AGA

Restarts the Copper from COP2LC.

## Bitfields

No data bitfields: accessing the address fires the strobe. Copperline handles CPU reads as well as writes; a read returns the undriven bus value.
