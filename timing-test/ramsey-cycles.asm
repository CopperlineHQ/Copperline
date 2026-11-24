; SPDX-License-Identifier: GPL-3.0-or-later
; A3000/A4000 with motherboard fast RAM. Run from a cold boot of the probe
; disk. Ramsey's DRAM cycle length in each of its modes, drawn as four bands
; whose heights are the raster lines 3000 passes of a loop take. Each pass
; reads two longwords of motherboard RAM; the loop itself runs from the
; instruction cache, so the RAM cycles are the only bus traffic.
;
;  1  standard mode           5 clocks a cycle at 25 MHz
;  2  page mode, one row      3 clocks (page hit)
;  3  page mode, two rows     7 clocks (page miss on every cycle)
;  4  Ramsey-07 skip mode     4 clocks (the -04 has no skip: as band 1)
;
; (Commodore Ramsey specification, "Standard Mode", "Page Mode" and control
; bit 4.) A control write takes effect at the next refresh, so each mode is
; given time to load before its loop starts at the top of line $21. Page
; mode needs static-column DRAM: the reads are never written back, and the
; control register is restored afterwards. No emulator reference.
CUST    equ     $dff000
PASSES  equ     3000
CAP     equ     44
RAMSEY  equ     $de0003
RAM     equ     $07e00000

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
        move.b  d6,d5
        andi.b  #$68,d5          ; standard: page, burst and wrap off
        cmpi.b  #$0f,$40(a4)     ; Ramsey-07: bit 4 is skip, off as well
        beq.s   .std
        move.b  d6,d0
        andi.b  #$10,d0          ; Ramsey-04: bit 4 is the DRAM width, kept
        or.b    d0,d5
.std:

        move.b  d5,d0
        lea     RAM,a1
        movea.l a1,a2
        bsr     measure
        move.b  d5,d0
        ori.b   #$01,d0          ; page mode, both reads in one row
        bsr     measure
        move.b  d5,d0
        ori.b   #$01,d0          ; page mode, the reads a megabyte apart
        lea     RAM+$100000,a2
        bsr     measure
        move.b  d5,d0
        ori.b   #$10,d0          ; skip
        movea.l a1,a2
        bsr     measure
        move.b  d6,d0
        bsr     setmode

        lea     clist(pc),a0
        move.l  a0,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$0200,$100(a6)
        move.w  #$8280,$096(a6)
.hold:  bra.s   .hold

; Loads control byte d0 and waits out a refresh interval.
setmode:
        move.b  d0,(a4)
        move.w  #3999,d1
.wait:  cmp.b   (a4),d0
        dbra    d1,.wait
        rts

; Loads mode d0, reads (a1) and (a2) PASSES times from the top of line $21
; and sets the height of the band at (a5) to the lines that took.
measure:
        bsr.s   setmode
.l20:   bsr.s   vpos
        cmpi.w  #$20,d1
        bne.s   .l20
.l21:   bsr.s   vpos
        cmpi.w  #$21,d1
        bne.s   .l21
        move.w  #PASSES-1,d0
.loop:  move.l  (a1),d1
        move.l  (a2),d1
        dbra    d0,.loop
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
