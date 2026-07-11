; DDFSTRT phase x BPLCON1 scroll placement probe (lo-res, FMODE=0).
; Bands of 8 lines set (DDFSTRT, BPLCON1) pairs while the copper re-anchors
; BPL1PT to the same marker bitmap every line. Bands 1/2 replicate the exact
; frame pair of Rampage's dot-cube pan (scroll wrap $FF->$00 with DDFSTRT
; $66->$68): on real hardware the pan is smooth, so their markers must sit
; 1 lo-res px apart. Bands 3/4 isolate the scroll term, bands 5-7 cover the
; arosddf1 photo case ($3C) with its on-grid neighbours.
; Marker: $FFFF $F000 at the start of the row.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000
NBAND  equ 7
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #$ffff,(a0)+
        move.w #$f000,(a0)+
        move.w #62-1,d0
.z:     clr.w (a0)+
        dbra d0,.z
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$009400c8,(a1)+   ; DDFSTOP $C8
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00920070,(a1)+   ; DDFSTRT preview above the bands
        move.l #$00e00004,(a1)+   ; BPL1PTH = $40000
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea pairs(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d4           ; DDFSTRT
        move.w (a2)+,d6           ; BPLCON1
        moveq #8-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07)
        move.w #$fffe,(a1)+
        move.w #$0092,(a1)+       ; DDFSTRT
        move.w d4,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w d6,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w #$0000,(a1)+
        add.w #$0100,d3
        dbra d5,.line
        dbra d2,.band
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
        cnop 0,2
pairs:  dc.w $66,$00ff            ; band 1: cube frame A (P=$66, S=15)
        dc.w $68,$0000            ; band 2: cube frame B (P=$68, S=0)
        dc.w $66,$0000            ; band 3
        dc.w $68,$00ff            ; band 4
        dc.w $38,$0000            ; band 5
        dc.w $3c,$0000            ; band 6: arosddf1 photo case
        dc.w $40,$0000            ; band 7
