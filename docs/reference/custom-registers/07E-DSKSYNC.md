# DSKSYNC
Offset: $07E
Access: write
Chipset: OCS/ECS/AGA

Sets the disk shifter's sync comparison word.

## Bitfields

- Bits 15-0: Raw sync word, commonly $4489 for AmigaDOS tracks.

Matches set the DSKSYNC interrupt request; ADKCON.WORDSYNC also uses the comparison to start read DMA.
