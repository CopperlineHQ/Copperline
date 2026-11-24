; SPDX-License-Identifier: GPL-3.0-or-later
; Address-decode map of the motherboard glue chip, drawn as eight colour
; bands from top to bottom. Green = a device answered at the probed address,
; blue = nothing did. Runs in place from chip RAM; no overlay is touched.
;
;  1  CIA-A at $A0E301 (only A12 low, away from $BFE001)
;  2  CIA-B at $A0D200 (only A13 low, away from $BFD000)
;  3  both CIAs from one word write at $BFC300 (A12 and A13 low)
;  4  CIA-A at $BFF301 (A12 and A13 high: neither chip is selected)
;  5  custom registers at $DFF200: INTENA written at $DFF29A reads back
;     through INTENAR
;  6  the Kickstart ROM at $E00000 (compared with $F80000)
;  7  the Kickstart ROM at $A80000
;  8  a ROM image at $B00000 ($F80000's, or $E00000's with a 1 MiB set)
;
; Gary (A500, A2000) asserts /VPA for all of $A00000-$BFFFFF and selects
; CIA-A by A12 low and CIA-B by A13 low, and mirrors the ROM at $E00000:
; bands 1 2 3 5 6 green with a 512 KiB Kickstart. Gayle (A600, A1200)
; decodes the two CIA pages alone and mirrors the ROM at $E00000 and
; $A80000-$B7FFFF: bands 5 6 7 8 green. Band 6 is blue under a 1 MiB ROM
; set, whose second part is what answers at $E00000; the CI goldens boot
; such a set. The chips see only RGA8-RGA1, so band 5 is green everywhere.
; vAmiga 4.4 (A500 OCS, Kickstart 1.3) draws bands 1 2 3 5 6 green, as this
; probe does on the same machine. It has no Gayle machine to compare.
CUST    equ     $dff000
GREEN   equ     $02c4
BLUE    equ     $024c

        lea     CUST,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        lea     bands+6(pc),a5

; 1: CIA-A DDRB through an address with only A12 low.
        lea     $bfe301,a0
        move.b  (a0),d7
        clr.b   (a0)
        move.b  #$5a,$a0e301
        cmpi.b  #$5a,(a0)
        seq     d0
        move.b  d7,(a0)
        bsr     mark

; 2: CIA-B DDRA through an address with only A13 low.
        lea     $bfd200,a0
        move.b  (a0),d7
        clr.b   (a0)
        move.b  #$c3,$a0d200
        cmpi.b  #$c3,(a0)
        seq     d0
        move.b  d7,(a0)
        bsr     mark

; 3: one word where both selects are low: CIA-B takes the high byte,
;    CIA-A the low one (DDRB of each).
        lea     $bfe301,a0
        lea     $bfd300,a1
        move.b  (a0),d7
        move.b  (a1),d6
        clr.b   (a0)
        clr.b   (a1)
        move.w  #$a55a,$bfc300
        cmpi.b  #$5a,(a0)
        seq     d0
        cmpi.b  #$a5,(a1)
        seq     d1
        and.b   d1,d0
        move.b  d6,(a1)
        move.b  d7,(a0)
        bsr     mark

; 4: both selects high: the write must reach neither chip.
        lea     $bfe301,a0
        move.b  (a0),d7
        clr.b   (a0)
        move.b  #$5a,$bff301
        cmpi.b  #$5a,(a0)
        seq     d0
        move.b  d7,(a0)
        bsr     mark

; 5: the register bank repeats every $200.
        move.w  #$8004,$dff29a
        move.w  $01c(a6),d1
        btst    #2,d1
        sne     d0
        move.w  #$7fff,$09a(a6)
        bsr     mark

; 6-8: ROM images. A match needs varying data, so two floating reads
;      cannot pass for a mirror.
        lea     $e00000,a0
        lea     $f80000,a1
        bsr     romcmp
        bsr     mark
        lea     $a80000,a0
        lea     $f80000,a1
        bsr     romcmp
        bsr     mark
        lea     $b00000,a0
        lea     $f80000,a1
        bsr     romcmp
        tst.b   d0
        bne.s   .b8
        lea     $b00000,a0
        lea     $e00000,a1
        bsr     romcmp
.b8:    bsr     mark

        lea     clist(pc),a0
        move.l  a0,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$0200,$100(a6)
        move.w  #$8280,$096(a6)
.hold:  bra.s   .hold

; d0.b nonzero = answered. Stores the band colour and steps to the next.
mark:   move.w  #BLUE,d1
        tst.b   d0
        beq.s   .set
        move.w  #GREEN,d1
.set:   move.w  d1,(a5)
        lea     16(a5),a5
        rts

; d0.b = $ff when 64 longwords spread through (a0) equal those at (a1)
; and are not all one value.
romcmp: moveq   #63,d2
        moveq   #0,d0
        moveq   #0,d4
        move.l  (a1),d5
.next:  move.l  (a0),d3
        cmp.l   (a1),d3
        bne.s   .no
        cmp.l   d5,d3
        beq.s   .same
        moveq   #1,d4
.same:  lea     $1ffc(a0),a0
        lea     $1ffc(a1),a1
        dbra    d2,.next
        tst.b   d4
        sne     d0
.no:    rts

BAND    macro
        dc.w    (\1<<8)|$07,$fffe,$0180,$0000
        dc.w    ((\1+16)<<8)|$07,$fffe,$0180,$0000
        endm

clist:  dc.w    $0180,$0000
bands:  BAND    $40
        BAND    $54
        BAND    $68
        BAND    $7c
        BAND    $90
        BAND    $a4
        BAND    $b8
        BAND    $cc
        dc.w    $ffff,$fffe
