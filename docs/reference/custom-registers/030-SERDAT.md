# SERDAT
Offset: $030
Access: write
Chipset: OCS/ECS/AGA

Queues a serial transmit word, including its stop bit.

## Bitfields

- Bit 9: Stop bit in 9-bit mode.
- Bit 8: Stop bit in 8-bit mode, or the ninth data bit in 9-bit mode.
- Bits 7-0: Data byte.

Paula sends a start bit, then shifts the word out from bit 0 up to its highest set bit, so the stop bit also sets the frame length. SERPER sets the bit period and the 8/9-bit word format.
