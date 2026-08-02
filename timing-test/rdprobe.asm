; Chip-bus access-phase probe for the 020.
;
; The real A1200 shows that a loop containing a chip-RAM access runs a WHOLE
; number of colour clocks per iteration, while a loop with no chip access
; runs any number at all. From the main timing-test disk and fwdprobe
; (clocks = ticks * 20.006 / 8192; 1 cck = 2 CPU clocks at 14.19 MHz):
;
;   with a chip access   main row 2  chip read   16.09 clk = 8.04 cck
;                        main row 10 chip write   8.11 clk = 4.05 cck
;                        main row 3  chip write   8.05 clk = 4.02 cck
;   without one          main row 4  reg move     9.01 clk = 4.51 cck
;                        fwd row 25  reg move     8.01 clk = 4.01 cck
;                        fwd row 24  bare dbra    6.01 clk = 3.01 cck
;                        fwd row 1   reg pair    11.01 clk = 5.51 cck
;
; The chip rows land on 8.04, 4.05 and 4.02; the others sit on halves as
; readily as wholes. That is the CPU synchronising to the chip clock: the
; access cannot start part way through a colour clock, so the wait absorbs
; the remainder and the loop period quantises.
;
; Copperline does not reproduce this. Its chip-read loop drifts instead of
; locking, and the SAME loop measures differently depending on where in the
; frame it starts -- 13.08 clocks here against 17.04 on the main disk, a 30%
; spread for identical code with all DMA off, where the real machine is
; stable at 16.09. This disk exists so that recalibration has real numbers
; to work from instead of the single row-2 datapoint.
;
; Rows (8192 iterations each; every loop's DBcc alignment is fixed by
; padding and listed as measured from the assembled binary):
;
;   row 0  move.w (a0),d0  + dbra     chip read      dbra%4=2
;   row 1  move.w (a0),d0  + dbra     chip read      dbra%4=0
;   row 2  move.w d1,(a0)  + dbra     chip write     dbra%4=2
;   row 3  move.w d1,(a0)  + dbra     chip write     dbra%4=0
;   row 4  move.w d2,d0    + dbra     no access      dbra%4=0   (anchor)
;   row 5  move.w d2,d0    + dbra     no access      dbra%4=2   (anchor)
;   row 6  two chip reads  + dbra                    dbra%4=2
;   row 7  two chip reads  + dbra                    dbra%4=0
;   row 8  read then write + dbra                    dbra%4=2
;   row 9  read then write + dbra                    dbra%4=0
;
; Rows 4 and 5 must read 0E6A and 0CD1 (9.01 and 8.01 clocks): they are the
; same loops as main-disk row 4 and fwdprobe row 25 and touch no chip RAM,
; so they validate the run before the chip rows are trusted.
;
; What the rows decide:
;   - rows 0/1 and 2/3: does a chip access absorb the one-clock branch
;     alignment difference, leaving both alignments equal and quantised, or
;     does it add to it? Row 0 against row 1 is the whole question for reads.
;   - rows 6/7: does a second access in the same loop cost the same as the
;     first, or does the chip port's turnaround make it dearer?
;   - rows 8/9: does a read following a posted write stall on that write
;     retiring, as Copperline models it?
;
; Copperline (m68k 0.5.0 plus the DBcc-alignment fix) currently reports
; 13.08 / 16.04 / 8.05 / 8.05 / 9.01 / 8.01 / 22.16 / 24.12 / 17.04 / 17.04
; clocks for rows 0-9. Any row where the real machine disagrees is a
; chip-bus modelling error; the anchors (4, 5) already agree.
;
; Loaded by boot.asm to $30000. Same CIA-A timer A harness, renderer and
; serial output as the other probe disks: a two-digit decimal row ID then
; the value as 8 hex digits, at a 7-scanline pitch inside the CRT-safe area.
; Scratch: $40000 screen, $48000 results, $60000 chip-RAM target.

CUST    equ     $dff000
ITERS   equ     $2000
NROWS   equ     10
SCREEN  equ     $40000
RESULTS equ     $48000
CHIPT   equ     $60000
boot:
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)
        move.w  #$7fff,$9c(a6)
        move.w  #$7fff,$96(a6)
        move.w  #$0f00,$180(a6)
        lea     SCREEN,a0
        move.w  #(40*256/4)-1,d0
.clrs   clr.l   (a0)+
        dbra    d0,.clrs
        move.l  #$00040000,d2
        move.l  #$00050000,d3
        lea     RESULTS,a3

        ; row 0: chip read, dbra longword aligned
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t00    move.w  (a0),d0
        dbra    d6,.t00
        bsr     tread
        move.l  d0,(a3)+

        ; row 1: chip read, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
.t01    move.w  (a0),d0
        dbra    d6,.t01
        bsr     tread
        move.l  d0,(a3)+

        ; row 2: chip write, dbra longword aligned
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t02    move.w  d1,(a0)
        dbra    d6,.t02
        bsr     tread
        move.l  d0,(a3)+

        ; row 3: chip write, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
.t03    move.w  d1,(a0)
        dbra    d6,.t03
        bsr     tread
        move.l  d0,(a3)+

        ; row 4: register move, dbra longword aligned (anchor = 9 clk)
        cnop    0,4
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t04    move.w  d2,d0
        dbra    d6,.t04
        bsr     tread
        move.l  d0,(a3)+

        ; row 5: register move, dbra at %4==2 (anchor = 8 clk)
        cnop    0,4
        bsr     tstart
        move.w  #ITERS-1,d6
.t05    move.w  d2,d0
        dbra    d6,.t05
        bsr     tread
        move.l  d0,(a3)+

        ; row 6: two chip reads, dbra longword aligned
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
.t06    move.w  (a0),d0
        move.w  (a0),d1
        dbra    d6,.t06
        bsr     tread
        move.l  d0,(a3)+

        ; row 7: two chip reads, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t07    move.w  (a0),d0
        move.w  (a0),d1
        dbra    d6,.t07
        bsr     tread
        move.l  d0,(a3)+

        ; row 8: read then write, dbra longword aligned
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
.t08    move.w  (a0),d0
        move.w  d1,(a0)
        dbra    d6,.t08
        bsr     tread
        move.l  d0,(a3)+

        ; row 9: read then write, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t09    move.w  (a0),d0
        move.w  d1,(a0)
        dbra    d6,.t09
        bsr     tread
        move.l  d0,(a3)+

        move.w  #$0ff0,$180(a6)
        bsr     render
        move.w  #$0170,$032(a6)
        lea     RESULTS,a2
        moveq   #NROWS-1,d4
.sl     move.l  (a2)+,d3
        moveq   #8-1,d6
.sh     rol.l   #4,d3
        move.l  d3,d0
        and.w   #$f,d0
        add.w   #'0',d0
        cmp.w   #'9',d0
        ble     .sok
        addq.w  #7,d0
.sok    bsr     sendb
        dbra    d6,.sh
        moveq   #13,d0
        bsr     sendb
        moveq   #10,d0
        bsr     sendb
        dbra    d4,.sl
        move.w  #$1000,$100(a6)
        move.w  #$0000,$102(a6)
        move.w  #$0000,$104(a6)
        move.w  #$0000,$108(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0000,$180(a6)
        move.w  #$0fff,$182(a6)
        move.w  #$8300,$096(a6)
.show:  bsr     syncframe
        move.l  #SCREEN,d0
        move.w  d0,$0e2(a6)
        swap    d0
        move.w  d0,$0e0(a6)
        bra     .show
sendb:
.tbe    move.w  $018(a6),d1
        btst    #13,d1
        beq     .tbe
        and.w   #$ff,d0
        or.w    #$100,d0
        move.w  d0,$030(a6)
        rts
getvpos:
        move.w  $004(a6),d0
        and.w   #1,d0
        lsl.w   #8,d0
        move.w  $006(a6),d1
        lsr.w   #8,d1
        or.w    d1,d0
        rts
syncframe:
.hi     bsr     getvpos
        cmp.w   #280,d0
        blo     .hi
.wrap   bsr     getvpos
        cmp.w   #280,d0
        bhs     .wrap
        rts
tstart:
        move.b  #$ff,$bfe401
        move.b  #$ff,$bfe501
        move.b  #$19,$bfee01
        rts
tread:
        move.b  #$08,$bfee01
        moveq   #0,d0
        move.b  $bfe501,d0
        lsl.w   #8,d0
        move.b  $bfe401,d0
        not.w   d0
        rts
render:
        lea     SCREEN,a1
        move.w  #40*256/4-1,d0
.rc:    clr.l   (a1)+
        dbra    d0,.rc
        lea     RESULTS,a2
        moveq   #0,d4
.rr:    move.l  (a2)+,d3
        move.w  d4,d0
        mulu    #280,d0
        lea     SCREEN,a5
        adda.l  d0,a5
        move.w  d4,d0
        ext.l   d0
        divu    #10,d0
        moveq   #0,d2
        bsr     .glyph
        swap    d0
        bsr     .glyph
        addq.w  #1,d2
        moveq   #7,d6
.rd:    rol.l   #4,d3
        move.l  d3,d0
        bsr     .glyph
        dbra    d6,.rd
        addq.w  #1,d4
        cmp.w   #NROWS,d4
        bne     .rr
        rts
.glyph:
        and.w   #$f,d0
        lsl.w   #3,d0
        lea     font(pc),a4
        adda.w  d0,a4
        move.l  a5,a1
        adda.w  d2,a1
        moveq   #6,d5
.rg:    move.b  (a4)+,(a1)
        adda.w  #40,a1
        dbra    d5,.rg
        addq.w  #1,d2
        rts
font:
        dc.b $70,$88,$98,$a8,$c8,$88,$70,$00
        dc.b $20,$60,$20,$20,$20,$20,$70,$00
        dc.b $70,$88,$08,$10,$20,$40,$f8,$00
        dc.b $70,$88,$08,$30,$08,$88,$70,$00
        dc.b $10,$30,$50,$90,$f8,$10,$10,$00
        dc.b $f8,$80,$f0,$08,$08,$88,$70,$00
        dc.b $30,$40,$80,$f0,$88,$88,$70,$00
        dc.b $f8,$08,$10,$20,$40,$40,$40,$00
        dc.b $70,$88,$88,$70,$88,$88,$70,$00
        dc.b $70,$88,$88,$78,$08,$10,$60,$00
        dc.b $70,$88,$88,$f8,$88,$88,$88,$00
        dc.b $f0,$88,$88,$f0,$88,$88,$f0,$00
        dc.b $70,$88,$80,$80,$80,$88,$70,$00
        dc.b $e0,$90,$88,$88,$88,$90,$e0,$00
        dc.b $f8,$80,$80,$f0,$80,$80,$f8,$00
        dc.b $f8,$80,$80,$f0,$80,$80,$80,$00
