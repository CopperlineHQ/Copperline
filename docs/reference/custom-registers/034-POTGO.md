# POTGO
Offset: $034
Access: write
Chipset: OCS/ECS/AGA

Controls the four potentiometer pins and starts analogue measurement.

## Bitfields

- Bits 15, 13, 11, 9: Output enables for port 2 Y/X and port 1 Y/X, respectively.
- Bits 14, 12, 10, 8: Output data for those pins.
- Bit 0: START, discharge the capacitors and begin a measurement.

The pins also carry mouse buttons and the CD32 pad serial interface.
