; SPDX-License-Identifier: GPL-3.0-or-later
; A1200/A4000/CD32. Width of the CPU port onto the custom registers, drawn
; as four bands whose heights are the raster lines 1000 passes of a loop
; take, one loop per band from top to bottom:
;
;  1  MOVE.L (chip RAM),Dn       one 32-bit cycle
;  2  MOVE.W INTENAR,Dn          one 16-bit cycle
;  3  MOVE.L INTENAR,Dn          two 16-bit cycles
;  4  MOVE.L Dn,FMODE            two 16-bit cycles (FMODE = 0, then NO-OP)
;
; Chip RAM is a 32-bit port beside Alice, but Gayle and Fat Gary end a
; register cycle with DSACK1 alone (Fat Gary specification, "Chip
; Registers"), so the CPU runs a long register access as two word cycles:
; band 3 is taller than band 2 by a whole register read cycle, data wait
; included, and band 4 is taller than a long write to chip RAM would be.
; Each loop starts at the top of line $21, so the heights do not depend
; on when the probe was entered.
; No emulator reference: vAmiga has no AGA machine.
CUST    equ     $dff000
PASSES  equ     1000
CAP     equ     44

        lea     CUST,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        clr.w   $1fc(a6)
        lea     bands(pc),a5
        lea     scratch(pc),a1
        moveq   #0,d2

        lea     ramlong(pc),a0
        bsr     measure
        lea     regword(pc),a0
        bsr     measure
        lea     reglong(pc),a0
        bsr     measure
        lea     wrlong(pc),a0
        bsr     measure

        lea     clist(pc),a0
        move.l  a0,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$0200,$100(a6)
        move.w  #$8280,$096(a6)
.hold:  bra.s   .hold

ramlong:
.l:     move.l  (a1),d1
        dbra    d0,.l
        rts
regword:
.l:     move.w  $01c(a6),d1
        dbra    d0,.l
        rts
reglong:
.l:     move.l  $01c(a6),d1
        dbra    d0,.l
        rts
wrlong:
.l:     move.l  d2,$1fc(a6)
        dbra    d0,.l
        rts

; Runs the loop at (a0) from the top of line $21 and sets the height of the
; band at (a5) to the lines it took, then steps a5 to the next band.
measure:
.l20:   bsr.s   vpos
        cmpi.w  #$20,d1
        bne.s   .l20
.l21:   bsr.s   vpos
        cmpi.w  #$21,d1
        bne.s   .l21
        move.w  #PASSES-1,d0
        jsr     (a0)
        bsr.s   vpos
        subi.w  #$21,d1
        cmpi.w  #CAP,d1
        bls.s   .ok
        moveq   #CAP,d1
.ok:    add.b   d1,8(a5)
        lea     16(a5),a5
        rts

vpos:   move.l  $004(a6),d1
        lsr.l   #8,d1
        andi.w  #$07ff,d1
        rts

BAND    macro
        dc.w    (\1<<8)|$07,$fffe,$0180,\2
        dc.w    (\1<<8)|$07,$fffe,$0180,$0000
        endm

clist:  dc.w    $0180,$0000
bands:  BAND    $30,$0fff
        BAND    $60,$02c4
        BAND    $90,$0fc2
        BAND    $c0,$0f4c
        dc.w    $ffff,$fffe
scratch: dc.l   0
