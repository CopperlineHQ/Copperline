; OCS DDFSTRT=$00 with a surviving SHW latch.
; Top band: standard $38/$D0 fetch. Middle: $00/$00, where a run-less
; line arms SHW and the next line starts at CCK 0 and drains at $D8.
; Bottom: $08/$08 control, one lores word to the right of the middle.
; Each line receives the same asymmetric row via BPL1MOD. Alternating
; empty lines in the middle/bottom are intentional sequencer behavior.
; In the middle band, rewrite DDFSTRT to $10 at hp $21 (behind the beam),
; then restore $00 at hp $D9. The armed run stays anchored at CCK 0;
; the register view at display time must not move its captured picture.
; External hardware verification remains outstanding for the zero case.
CUST    equ $dff000
BMP     equ $40000
CLIST   equ $60000
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #8192-1,d0
.zero:  clr.w (a0)+
        dbra d0,.zero
        move.w #$ffff,BMP+16
        move.w #$ff0f,BMP+24
        move.w #$f000,BMP+40
        lea CLIST,a1
        move.l #$01001200,(a1)+
        move.l #$01020000,(a1)+
        move.l #$01040024,(a1)+
        move.l #$01800008,(a1)+
        move.l #$01820fff,(a1)+
        move.l #$008e2c81,(a1)+
        move.l #$00902cc1,(a1)+
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+
        move.l #$00920038,(a1)+
        move.l #$009400d0,(a1)+
        move.l #$0108ffd8,(a1)+ ; standard 20 words
        move.l #$3cd9fffe,(a1)+
        move.l #$00920000,(a1)+
        move.l #$00940000,(a1)+
        move.l #$0108ffc8,(a1)+ ; zero-origin drain: 28 words
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+
        move.w #$3d00,d3
        moveq #64-1,d2
.line:  move.w d3,d0
        or.w #$0021,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$00920010,(a1)+
        move.w d3,d0
        or.w #$00d9,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$00920000,(a1)+
        add.w #$0100,d3
        dbra d2,.line
        move.l #$7cd9fffe,(a1)+
        move.l #$00920008,(a1)+
        move.l #$00940008,(a1)+
        move.l #$0108ffca,(a1)+ ; origin $08 drain: 27 words
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+
        move.l #$bcd9fffe,(a1)+
        move.l #$00920038,(a1)+
        move.l #$009400d0,(a1)+
        move.l #$0108ffd8,(a1)+
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)
.loop:  bra.s .loop
