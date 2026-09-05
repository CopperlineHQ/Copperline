# SERDAT
Offset: $030
Access: write
Chipset: OCS/ECS/AGA

Queues a serial transmit word, including its stop bit.

## Bitfields

- Bits 8-0: Data and first stop-bit position for the selected word length.
- Bit 9: Stop bit for 9-bit mode.

For 8-bit transmission, set bit 8 above the data byte. SERPER selects the bit period and 8/9-bit format.
