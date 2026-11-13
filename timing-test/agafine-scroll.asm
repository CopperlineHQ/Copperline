; AGA sub-pixel playfield scroll and sprite position.
;
; Lisa's BPLCON1 scroll counters are eight bits wide in 35 ns units. The
; OCS nibbles are H5-H2 (whole lo-res pixels); H1/H0, bits 9-8 for
; playfield 1, delay a lo-res playfield by 70 and 35 ns. SPRxCTL bit 4
; and bit 3 move a sprite by the same two steps in every display mode.
;
; One lo-res plane holds a vertical bar 64 pixels wide, repeated on every
; line by a negative modulo. The Copper rewrites BPLCON1 and re-arms
; sprite 0 (eight lo-res pixels, sprite DMA off) at the top of each of
; eight bands of sixteen lines:
;
;   band  BPLCON1  bar delay (35 ns)   SPR0CTL  sprite offset (35 ns)
;    0    $0000     0                  $0000    0
;    1    $0100     1                  $0008    1
;    2    $0200     2                  $0010    2
;    3    $0300     3                  $0018    3
;    4    $0001     4                  $0000    0
;    5    $0101     5                  $0008    1
;    6    $0302    11                  $0010    2
;    7    $0005    20                  $0018    3
;
; Expected render: both edges of the white bar step right band by band by
; the delays above, and the red sprite by its offsets. A framebuffer pixel
; is 70 ns, so an odd delay shows as a half-bright column at each edge. A
; model that drops H1/H0 shows bands 0-3 identical and band 5 equal to
; band 4; one that ignores the sprite bits outside super-hi-res shows the
; sprite at one position in every band.
;
; No reference emulator: vAmiga 5 implements neither BPLCON1 H1/H0 nor
; SPRxCTL bit 3. The expectations follow the AGA register documentation
; and WinUAE's position and scroll decode.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        move.w #$0000,$1fc(a6)    ; FMODE: 16-bit fetches, 16-pixel sprites

        ; One bitmap row: pixels 64-127 set.
        lea BMP,a0
        moveq #20-1,d0
.cz:    clr.w (a0)+
        dbra d0,.cz
        lea BMP+8,a0
        move.l #$ffffffff,(a0)+
        move.l #$ffffffff,(a0)+

        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$0108ffd8,(a1)+   ; BPL1MOD -40: repeat the row
        move.l #$010affd8,(a1)+   ; BPL2MOD
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$010c0011,(a1)+   ; BPLCON4: sprite colours from 16
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$01a20f00,(a1)+   ; COLOR17 red
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+

        lea    table(pc),a2
        moveq  #8-1,d7
        move.w #$4007,d6          ; WAIT line $40
.band:  move.w d6,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w (a2)+,(a1)+
        move.w #$0140,(a1)+       ; SPR0POS: h-byte $A0
        move.w #$00a0,(a1)+
        move.w #$0142,(a1)+       ; SPR0CTL: position bits (disarms)
        move.w (a2)+,(a1)+
        move.w #$0146,(a1)+       ; SPR0DATB
        move.w #$0000,(a1)+
        move.w #$0144,(a1)+       ; SPR0DATA (arms)
        move.w #$ff00,(a1)+
        add.w  #$1000,d6          ; the next band, sixteen lines down
        dbra   d7,.band

        move.w d6,(a1)+           ; line $C0: sprite off, scroll back to 0
        move.w #$fffe,(a1)+
        move.l #$01420000,(a1)+
        move.l #$01020000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN (sprite DMA off)
.halt:  bra.s .halt

table:  dc.w $0000,$0000
        dc.w $0100,$0008
        dc.w $0200,$0010
        dc.w $0300,$0018
        dc.w $0001,$0000
        dc.w $0101,$0008
        dc.w $0302,$0010
        dc.w $0005,$0018
