; AGA dual-playfield palette mask. Only plane 2 contains ones. PF2
; colour 1 is at palette entry 9 (green). Mask $04 selects entry 13
; (red) if applied to the resolved address. Applying it first turns
; on plane 3, which makes PF1 colour 2 (blue) win the priority test.
        lea $dff000,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)
        lea $40000,a0
        lea planevals(pc),a1
        moveq #5,d0
.plane: move.w (a1)+,d1
        moveq #19,d2
.word:  move.w d1,(a0)+
        dbra d2,.word
        dbra d0,.plane
        lea list(pc),a0
        move.l a0,$080(a6)
        move.w d0,$088(a6)
        move.w #$8380,$096(a6)
.hold:  bra.s .hold
planevals:
        dc.w 0,$ffff,0,0,0,0
list:
        dc.w $08e,$2c81,$090,$2cc1,$092,$38,$094,$d0
        dc.w $102,0,$104,0,$108,-40,$10a,-40,$1fc,0
        dc.w $0e0,4,$0e2,0,$0e4,4,$0e6,40,$0e8,4,$0ea,80
        dc.w $0ec,4,$0ee,120,$0f0,4,$0f2,160,$0f4,4,$0f6,200
        dc.w $106,$0c00,$180,0,$182,$135,$184,$00f,$192,$0f0,$19a,$f00
        dc.w $106,$0e00,$182,$246,$106,$0c00
        dc.w $100,$2601,$10c,$0011
        dc.w $8001,$fffe,$10c,$0411
        dc.w $ffff,$fffe
