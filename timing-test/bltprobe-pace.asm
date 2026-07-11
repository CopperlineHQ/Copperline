; CPU pacing bars under blitter loads (BLTPRI fence / slot-cadence probe).
;
; For each blit class the CPU busy-polls DMACONR BBUSY and counts loop
; iterations until the blit completes; the count is drawn as a white bar
; (1 word per 8 iterations, clamped to the 20-word display row). The bar length
; therefore encodes how many chip-bus cycles the CPU wins while that blit
; runs -- the quantity the BLTPRI/BLS model governs. Regression example:
; the whole-blit BLS fence collapsed the fill and line bars to ~1 word,
; which starved Rampage's line-heavy frame handler ("present" flicker and
; music slowdown); the warm-up-window fence restores them.
;
;   rows 16-23:  full-width scale reference (always 20 words)
;   rows 32-39:  A->D copy 20x20 under BLTPRI   (saturated: shortest bar)
;   rows 48-55:  A->D DESC fill 20x20 under BLTPRI (fill-idle cycles free)
;   rows 64-71:  100-px line blit under BLTPRI  (2 of 4 cycles free)
;   rows 80-87:  100-px line blit, BLTPRI clear (adds the starvation yield)
CUST   equ $dff000
BMP    equ $40000
SRC    equ $50000
DST    equ $52000
FDST   equ $53000
LDST   equ $54000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #10240-1,d0
.cz:    clr.w (a0)+
        dbra d0,.cz
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$87c0,$96(a6)     ; SET DMAEN|BPLEN|COPEN|BLTEN|BLTPRI
        move.w #$ffff,$44(a6)     ; BLTAFWM
        move.w #$ffff,$46(a6)     ; BLTALWM

        ; scale reference bar
        moveq #20,d3
        moveq #16,d2
        bsr.w drawbar

        ; ---- A->D copy 20x20 words under BLTPRI ----
        moveq #0,d7
        move.w #$09f0,$40(a6)     ; BLTCON0 USEA|USED, LF=A
        clr.w  $42(a6)            ; BLTCON1
        move.l #SRC,$50(a6)       ; BLTAPT
        move.l #DST,$54(a6)       ; BLTDPT
        clr.w  $64(a6)            ; BLTAMOD
        clr.w  $66(a6)            ; BLTDMOD
        move.w #1300,$58(a6)      ; BLTSIZE h=20 w=20
.w1:    addq.l #1,d7
        btst   #6,$02(a6)         ; DMACONR BBUSY
        bne.s  .w1
        move.l d7,d3
        lsr.l  #3,d3
        addq.w #1,d3
        moveq  #32,d2
        bsr.w  drawbar

        ; ---- A->D descending fill 20x20 words under BLTPRI ----
        moveq #0,d7
        move.w #$09f0,$40(a6)
        move.w #$0012,$42(a6)     ; BLTCON1 DESC|IFE
        move.l #SRC+798,$50(a6)   ; end addresses for the descending pass
        move.l #FDST+798,$54(a6)
        clr.w  $64(a6)
        clr.w  $66(a6)
        move.w #1300,$58(a6)
.w2:    addq.l #1,d7
        btst   #6,$02(a6)
        bne.s  .w2
        move.l d7,d3
        lsr.l  #3,d3
        addq.w #1,d3
        moveq  #48,d2
        bsr.w  drawbar

        ; ---- 100-px line blit under BLTPRI ----
        bsr.w  lineregs
        move.w #6402,$58(a6)      ; BLTSIZE h=100 w=2
        moveq  #0,d7
.w3:    addq.l #1,d7
        btst   #6,$02(a6)
        bne.s  .w3
        move.l d7,d3
        lsr.l  #3,d3
        addq.w #1,d3
        moveq  #64,d2
        bsr.w  drawbar

        ; ---- the same line blit with BLTPRI clear (nice mode) ----
        move.w #$0400,$96(a6)     ; CLR BLTPRI
        bsr.w  lineregs
        move.w #6402,$58(a6)
        moveq  #0,d7
.w4:    addq.l #1,d7
        btst   #6,$02(a6)
        bne.s  .w4
        move.l d7,d3
        lsr.l  #3,d3
        addq.w #1,d3
        moveq  #80,d2
        bsr.w  drawbar

.halt:  bra.s  .halt

; Program a 100x30 line blit (octant 0, dx=100 dy=30) into LDST.
lineregs:
        move.w #$0bca,$40(a6)     ; BLTCON0 start=0 USEA|USEC|USED LF=$CA
        move.w #$0001,$42(a6)     ; BLTCON1 LINE
        move.w #-280,$64(a6)      ; BLTAMOD = 4*(dy-dx)
        move.w #120,$62(a6)       ; BLTBMOD = 4*dy
        move.l #-80,$50(a6)       ; BLTAPT accumulator = 4*dy-2*dx
        move.w #$8000,$74(a6)     ; BLTADAT
        move.w #$ffff,$72(a6)     ; BLTBDAT
        move.l #LDST,$48(a6)      ; BLTCPT
        move.l #LDST,$54(a6)      ; BLTDPT
        move.w #80,$60(a6)        ; BLTCMOD (row bytes)
        move.w #80,$66(a6)        ; BLTDMOD
        rts

; Draw a bar: d2 = first bitmap row, d3 = width in words (clamped 1..20),
; 8 rows tall (40-byte rows: the standard DDF window fetches 20 words).
drawbar:
        tst.w  d3
        bgt.s  .lo
        moveq  #1,d3
.lo:    cmp.w  #20,d3
        ble.s  .hi
        moveq  #20,d3
.hi:    lea    BMP,a0
        move.w d2,d0
        mulu   #40,d0
        adda.l d0,a0
        moveq  #8-1,d1
.row:   move.l a0,a2
        move.w d3,d0
        subq.w #1,d0
.wds:   move.w #$ffff,(a2)+
        dbra   d0,.wds
        lea    40(a0),a0
        dbra   d1,.row
        rts
