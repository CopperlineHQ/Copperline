# SERDATR
Offset: $018
Access: read
Chipset: OCS/ECS/AGA

Reads serial receive data and transmit/receive status.

## Bitfields

- Bit 15: OVRUN, receive overrun.
- Bit 14: RBF, receive buffer full.
- Bit 13: TBE, transmit buffer empty.
- Bit 12: TSRE, transmit shift register empty.
- Bit 11: RXD, synchronized receive-pin level.
- Bits 9-0: Received data and stop bits.

Acknowledge RBF through INTREQ; reading this register does not clear it.
