; SPDX-License-Identifier: GPL-3.0-or-later
; A500, A1000, A2000 or CDTV (Gary's /VPA decode). The data lanes and the
; cycle of the CIA range, drawn as five colour bands from top to bottom.
; Green = the behaviour below was seen, blue = it was not. Runs in place
; from chip RAM; no overlay is touched.
;
;  1  a long read at $A0C300 (both chips selected) is two word cycles:
;     CIA-B's DDRB in D15-D8 and CIA-A's in D7-D0, twice
;  2  a long write there is two word cycles too: the second word is the
;     one left in the two registers
;  3  CIA-A drives D7-D0 only: the odd byte of $A0E300 (A12 low alone)
;     reads its DDRB and the even byte does not follow it
;  4  CIA-B drives D15-D8 only: the even byte of $A0D300 (A13 low alone)
;     reads its DDRB and the odd byte does not follow it
;  5  /VPA depends on A23-A21 alone: a read of $BFF301, which selects
;     neither chip, runs the same E-clock cycle as a read of $BFE301, and
;     a longer one than a read of the unmapped $9FF301
;
; Bands 3 and 4 read the undriven lane twice from the same code with two
; different register values. A chip that answered would return the two
; values; a floating lane returns the same charge both times. Band 5 times
; 1024 byte reads of each address with CIA-B timer A, which counts E
; clocks: an E-clock cycle paces the loop at 20 CPU clocks a turn, two
; timer counts, where an ordinary cycle lets it run at 18.
;
; Gary: all five green. Gayle and Fat Gary decode the two standard pages
; alone, so nothing answers at the first four addresses and $BFF301 is an
; ordinary cycle: all five blue.
; vAmiga is a valid reference: it selects the chips and their lanes the
; same way (Memory::peekCIA8, peekCIA16, pokeCIA16) and synchronises every
; access of the range with the E clock.
CUST    equ     $dff000
GREEN   equ     $02c4
BLUE    equ     $024c
TURNS   equ     1024

        lea     CUST,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        lea     bands+6(pc),a5
        lea     $bfe301,a0       ; CIA-A DDRB
        lea     $bfd300,a1       ; CIA-B DDRB
        move.b  (a0),d7
        move.b  (a1),d6

; 1: long read with both selects low.
        move.b  #$34,(a0)
        move.b  #$12,(a1)
        move.l  $a0c300,d1
        cmpi.l  #$12341234,d1
        seq     d0
        bsr     mark

; 2: long write with both selects low.
        move.l  #$11223344,$a0c300
        cmpi.b  #$44,(a0)
        seq     d0
        cmpi.b  #$33,(a1)
        seq     d1
        and.b   d1,d0
        bsr     mark

; 3: the upper lane where CIA-A alone is selected.
        lea     $a0e300,a2
        move.b  #$5a,(a0)
        bsr     lane
        move.b  d0,d2
        move.b  #$a5,(a0)
        bsr     lane
        cmp.b   d0,d2
        seq     d0
        cmpi.b  #$a5,1(a2)
        seq     d1
        and.b   d1,d0
        bsr     mark

; 4: the lower lane where CIA-B alone is selected.
        lea     $a0d301,a2
        move.b  #$5a,(a1)
        bsr     lane
        move.b  d0,d2
        move.b  #$a5,(a1)
        bsr     lane
        cmp.b   d0,d2
        seq     d0
        cmpi.b  #$a5,-1(a2)
        seq     d1
        and.b   d1,d0
        bsr     mark
        move.b  d6,(a1)
        move.b  d7,(a0)

; 5: the cycle where neither chip is selected.
        lea     $bfe301,a2
        bsr     timed
        move.w  d0,d5            ; a CIA cycle
        lea     $bff301,a2
        bsr     timed
        move.w  d0,d4            ; neither chip selected
        lea     $9ff301,a2
        bsr     timed            ; d0 = an ordinary cycle
        sub.w   d4,d0            ; ordinary - unselected, negative
        cmpi.w  #-64,d0
        slt     d0
        sub.w   d4,d5            ; CIA - unselected, within one count
        addq.w  #1,d5
        cmpi.w  #2,d5
        sls     d1
        and.b   d1,d0
        bsr     mark

        lea     clist(pc),a0
        move.l  a0,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$0200,$100(a6)
        move.w  #$8280,$096(a6)
.hold:  bra.s   .hold

; d0.b nonzero = seen. Stores the band colour and steps to the next.
mark:   move.w  #BLUE,d1
        tst.b   d0
        beq.s   .set
        move.w  #GREEN,d1
.set:   move.w  d1,(a5)
        lea     8(a5),a5
        rts

; d0.b = the byte at (a2). One routine, so both reads of a lane follow
; the same bus history.
lane:   move.b  (a2),d0
        rts

; d0.w = E clocks that TURNS byte reads of (a2) take, on CIA-B timer A.
timed:  move.b  #$00,$bfde00     ; stop
        move.b  #$ff,$bfd400
        move.b  #$ff,$bfd500
        move.w  #TURNS-1,d1
        move.b  #$11,$bfde00     ; load and start, continuous
.turn:  tst.b   (a2)
        dbra    d1,.turn
        move.b  #$00,$bfde00
        moveq   #0,d0
        move.b  $bfd500,d0
        lsl.w   #8,d0
        move.b  $bfd400,d0
        not.w   d0               ; $ffff - count
        rts

BAND    macro
        dc.w    (\1<<8)|$07,$fffe,$0180,$0000
        endm

clist:  dc.w    $0180,$0000
bands:  BAND    $40
        BAND    $54
        BAND    $68
        BAND    $7c
        BAND    $90
        dc.w    $a407,$fffe,$0180,$0000
        dc.w    $ffff,$fffe
