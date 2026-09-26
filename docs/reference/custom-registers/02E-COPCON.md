# COPCON
Offset: $02E
Access: write
Chipset: OCS/ECS/AGA

Controls the Copper's access to low custom-register addresses.

## Bitfields

- Bit 1: CDANG, let the Copper write registers below $080.

With CDANG clear, the Copper can write only $080 and above. Setting CDANG lowers the limit to $040 on OCS and removes it on ECS/AGA. A MOVE to a protected register stops the Copper.
