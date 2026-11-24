; SPDX-License-Identifier: GPL-3.0-or-later
; A3000/A4000 with motherboard fast RAM. Run from a cold boot of the probe
; disk. Fat Gary's ROM cycle length, drawn as four bands whose heights are
; the raster lines 1800 passes of a loop take. Each pass makes two reads;
; the loop itself runs from the instruction cache, so those reads are the
; only bus traffic.
;
;  1  motherboard RAM, longwords      Ramsey standard mode: 5 clocks a cycle
;  2  ROM, aligned longwords          one ROM cycle each
;  3  ROM, words                      one ROM cycle each: as tall as band 2
;  4  ROM, longwords at an odd word   two ROM cycles each
;
; Gary ends a ROM cycle with *STERM after 5, 6, 7 or 8 CPU clocks, set by the
; jumpers on ROMJP1 and ROMJP0 (Fat Gary specification, "ROM Timing"). The
; 25 MHz A3000 boards ship with J151 and J152 both at 1-2, 8 clocks (280 ns,
; the only 25 MHz setting that covers the 250 ns of the system ROMs); the
; A4000 ships with J151 at 1-2, 6 clocks (200 ns). The ROM is a 32-bit port,
; so bands 2 and 3 are equal and band 4 costs twice the cycles: band 4 less
; band 2 is two ROM cycles a pass, 12 clocks on the A4000 and 16 on the
; A3000. (Band 1 is a shorter cycle than the CPU's own time for the read, so
; it is not a clean five clocks to subtract.) No emulator reference.
CUST    equ     $dff000
PASSES  equ     1800
CAP     equ     46
RAMSEY  equ     $de0003
RAM     equ     $07e00000
ROM     equ     $00f80010

        machine 68040
        move.w  296(a6),d7       ; ExecBase AttnFlags
        jsr     -150(a6)         ; exec.library/SuperState
        move.w  #$2700,sr
        lea     $60000,sp
        moveq   #0,d0
        btst    #3,d7            ; 68040
        beq.s   .m030
        cpusha  bc
        movec   d0,cacr
        movec   d0,tc
        move.l  #$00008000,d0    ; instruction cache on, data cache off
        movec   d0,cacr
        bra.s   .physical
.m030:  machine 68030
        movec   d0,cacr
        clr.l   -(sp)
        pmove   (sp),tc
        addq.l  #4,sp
        moveq   #$09,d0          ; clear and enable the instruction cache
        movec   d0,cacr
.physical:
        lea     CUST,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        lea     bands(pc),a5
        lea     RAMSEY,a4
        move.b  (a4),d6          ; the mode to restore
        move.b  d6,d0
        andi.b  #$68,d0          ; standard: page, burst and wrap off
        cmpi.b  #$0f,$40(a4)     ; Ramsey-07: bit 4 is skip, off as well
        beq.s   .std
        move.b  d6,d1
        andi.b  #$10,d1          ; Ramsey-04: bit 4 is the DRAM width, kept
        or.b    d1,d0
.std:   bsr     setmode

        lea     RAM,a1
        lea     RAM+$40,a2
        lea     longs(pc),a0
        bsr     measure
        lea     ROM,a1
        lea     ROM+$40,a2
        lea     longs(pc),a0
        bsr     measure
        lea     words(pc),a0
        bsr     measure
        lea     ROM+2,a1
        lea     ROM+$42,a2
        lea     longs(pc),a0
        bsr     measure

        move.b  d6,d0
        bsr     setmode
        lea     clist(pc),a0
        move.l  a0,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$0200,$100(a6)
        move.w  #$8280,$096(a6)
.hold:  bra.s   .hold

longs:
.l:     move.l  (a1),d1
        move.l  (a2),d1
        dbra    d0,.l
        rts
words:
.l:     move.w  (a1),d1
        move.w  (a2),d1
        dbra    d0,.l
        rts

; Loads control byte d0 and waits out a refresh interval.
setmode:
        move.b  d0,(a4)
        move.w  #3999,d1
.wait:  cmp.b   (a4),d0
        dbra    d1,.wait
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
