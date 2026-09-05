# COPCON
Offset: $02E
Access: write
Chipset: OCS/ECS/AGA

Controls the Copper's access to low custom-register addresses.

## Bitfields

- Bit 1: CDANG, permit Copper writes to the blitter register range.

On OCS, clearing CDANG restricts writes to $080 and above; setting it lowers the boundary to $040. ECS/AGA extend the permitted range when CDANG is set.
