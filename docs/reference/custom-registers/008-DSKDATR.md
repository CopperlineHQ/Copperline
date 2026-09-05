# DSKDATR
Offset: $008
Access: read
Chipset: OCS/ECS/AGA

Reads a raw disk data word from the selected ready track.

## Bitfields

- Bits 15-0: Raw encoded track word.

Copperline retains the last word when no ready track supplies a new value. DSKBYTR provides byte-ready and sync status.
