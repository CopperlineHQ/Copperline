; AGA bitplane DMA bandwidth probe.
;
; Seven 24-line SHRES/HIRES bands request counts on both sides of Alice's
; resolution/FMODE ceilings. Every plane points at an all-one source; valid
; fetches therefore display palette entry 3, 15, or 255, while each invalid
; band clears BPLDAT1-8 before DDFSTRT and must remain COLOR00:
;   SHRES FMODE0: 2 valid / 3 invalid
;   SHRES FMODE1: 4 valid / 5 invalid
;   HIRES FMODE0: 4 valid / 5 invalid
;   SHRES FMODE3: 8 valid
; Cross-checked with vAmigaTS Agnus/DDF/AGADDF and Denise/Modes/shres.
CUST    equ $dff000
BMP     equ $40000
CLIST   equ $60000

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        lea BMP,a0
        move.w #$7fff,d0
.fill:  move.w #$ffff,(a0)+
        dbra d0,.fill

        ; Palette: entry 0 dark, 3 green, 15 cyan, 255 white.
        clr.w $106(a6)
        move.w #$0008,$180(a6)
        move.w #$00f0,$186(a6)
        move.w #$00ff,$19e(a6)
        move.w #$e000,$106(a6)
        move.w #$0fff,$1be(a6)
        clr.w $106(a6)

        lea CLIST,a1
        move.l #$008e4081,(a1)+
        move.l #$0090e8c1,(a1)+
        move.l #$00920038,(a1)+
        move.l #$009400d0,(a1)+
        clr.w d0
        move.w #$00e0,d1
        moveq #7,d2
.ptr:   move.w d1,(a1)+           ; BPLxPTH
        move.w #$0004,(a1)+
        addq.w #2,d1
        move.w d1,(a1)+           ; BPLxPTL
        move.w d0,(a1)+
        addq.w #2,d1
        dbra d2,.ptr
        move.l #$01080000,(a1)+
        move.l #$010a0000,(a1)+

        lea bands(pc),a2
        moveq #7-1,d7
.band:  move.w (a2)+,(a1)+        ; WAIT word
        move.w #$fffe,(a1)+
        move.w #$01fc,(a1)+
        move.w (a2)+,(a1)+        ; FMODE
        move.w #$0100,(a1)+
        move.w (a2)+,(a1)+        ; BPLCON0
        move.w (a2)+,d6           ; clear flag (all bands here)
        tst.w d6
        beq.s .noclear
        move.w #$0110,d1
        moveq #8-1,d2
.clear: move.w d1,(a1)+
        clr.w (a1)+
        addq.w #2,d1
        dbra d2,.clear
.noclear:
        dbra d7,.band
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$080(a6)
        move.w d0,$088(a6)
        move.w #$8380,$096(a6)
.loop:  bra.s .loop

bands:
        ; Program each band on the preceding line: even the eight BPLDAT
        ; clears then finish before the band's DDFSTRT comparator.
        dc.w $3f07,$0000,$2241,1 ; SHRES FMODE0, 2 planes: entry 3
        dc.w $5707,$0000,$3241,1 ; SHRES FMODE0, 3 planes: no fetch
        dc.w $6f07,$0001,$4241,1 ; SHRES FMODE1, 4 planes: entry 15
        dc.w $8707,$0001,$5241,1 ; SHRES FMODE1, 5 planes: no fetch
        dc.w $9f07,$0000,$c200,1 ; HIRES FMODE0, 4 planes: entry 15
        dc.w $b707,$0000,$d200,1 ; HIRES FMODE0, 5 planes: no fetch
        dc.w $cf07,$0003,$0251,1 ; SHRES FMODE3, 8 planes: entry 255
