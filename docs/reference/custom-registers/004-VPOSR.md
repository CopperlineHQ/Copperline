# VPOSR
Offset: $004
Access: read
Chipset: OCS/ECS/AGA

Reads the long-field and long-line flags, the Agnus identification, and the high bits of the vertical beam position.

## Bitfields

- Bit 15: LOF, long-field flag.
- Bits 14-8: Agnus identification: $00 PAL / $10 NTSC on OCS, $20 / $30 on an ECS Agnus, $23 / $33 on Alice.
- Bit 7: LOL, long-line flag.
- Bits 2-1: Vertical position bits 10-9 (ECS Agnus and Alice; zero on OCS).
- Bit 0: Vertical position bit 8; VHPOSR supplies bits 7-0.

Bits 2-0 are the top of the same counter as VHPOSR's high byte and step in the same colour clock. A programmable scan (BEAMCON0.VARBEAMEN) taller than 512 lines needs bits 2-1 to tell line 0 from line 512. While BPLCON0.LPEN holds a light-pen position and BEAMCON0.LPENDIS is clear, bits 2-0 come from that position; while BPLCON0.ERSY is set with no genlock sync source present (`[chipset] genlock` or a sync-supplying video-slot card), LOF, LOL, and bits 2-0 keep their values from when ERSY was set.
