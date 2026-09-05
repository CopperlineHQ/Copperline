# POTGOR
Offset: $016
Access: read
Chipset: OCS/ECS/AGA

Reads potentiometer pin levels and output-enable latches.

## Bitfields

- Bits 15, 13, 11, 9: POTGO output-enable latches.
- Bits 14, 12, 10, 8: Sensed levels of port 2 Y/X and port 1 Y/X.

A pressed button can pull a pin low even when its output data bit is high.
