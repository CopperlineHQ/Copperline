; A1200: poll the last destination word during seven-plane FMODE=0 DMA.
; Each row group shows the number of zero reads before the final $FFFF
; appeared (binary, MSB on the left). Vary the blit width and starting
; position to move CPU reads across Alice's terminal D cycle.
; vAmiga's AGA profile lacks the 68020 bus unit and board BLS gate.
CUSTOM  equ $dff000
BITMAP  equ $40000
SOURCE  equ $50000
LASTD   equ $58000
        lea CUSTOM,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)
        lea BITMAP,a0
        move.w #2559,d0
.clear: clr.l (a0)+
        dbra d0,.clear
        lea measure(pc),a0
        move.l a0,$080(a6)
        move.w d0,$088(a6)
        move.w #$83c0,$096(a6)
        move.w #$0fff,$040(a6)  ; ABCD, minterm always one
        clr.w $042(a6)
        move.w #$ffff,$044(a6)
        move.w #$ffff,$046(a6)
        lea LASTD,a2
        lea BITMAP,a3
        moveq #0,d6
.trial: move.l #SOURCE,$050(a6)
        move.l #SOURCE,$04c(a6)
        move.l #SOURCE,$048(a6)
        move.l a2,d0
        sub.w d6,d0
        sub.w d6,d0
        move.l d0,$054(a6)
        clr.w (a2)
        move.w d6,d2
        addi.w #65,d2            ; one line, width d6+1
.leaveline:
        cmpi.b #$70,$006(a6)
        beq.s .leaveline
.waitline:
        cmpi.b #$70,$006(a6)
        bne.s .waitline
        move.w d6,d1
        andi.w #7,d1
.delay: nop
        dbra d1,.delay
        moveq #0,d0
        move.w d2,$058(a6)
.poll:  tst.w (a2)
        bne.s .got
        addq.w #1,d0
        bra.s .poll
.got:   moveq #7,d1
.row:   move.w d0,d2
        moveq #15,d3
        move.l a3,a0
.bit:   add.w d2,d2
        scs d4
        move.b d4,(a0)+
        move.b d4,(a0)+
        dbra d3,.bit
        lea 40(a3),a3
        dbra d1,.row
        addq.w #1,d6
        cmpi.w #32,d6
        bne.s .trial
        lea display(pc),a0
        move.l a0,$080(a6)
.done:  bra.s .done

measure:
        dc.w $100,$7200,$102,0,$104,0,$1fc,0
        dc.w $08e,$2c81,$090,$2cc1,$092,$38,$094,$d0
        dc.w $108,-40,$10a,-40
reg     set $0e0
        rept 7
        dc.w reg,4,reg+2,0
reg     set reg+4
        endr
        dc.l $fffffffe
display:
        dc.w $100,$1200,$180,$013,$182,$fff
        dc.w $0e0,4,$0e2,0,$108,0,$10a,0
        dc.l $fffffffe
