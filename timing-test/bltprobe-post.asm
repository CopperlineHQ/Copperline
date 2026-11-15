; 68020+ chip writes under an all-channel blit (posted-write / BLS probe).
;
; A 68020's bus unit posts a chip write: execution carries on while the
; transfer waits for a chip slot. The write is still a CPU bus request, so
; with BLTPRI clear the blitter passes it over three times and then gives
; up a bus cycle, as it does for a read. This probe starts a blit that
; uses the chip bus on every cycle (A, B, C and D enabled), makes 256 chip
; writes from a DBRA loop, and then counts DMACONR polls until BBUSY
; clears. The poll count is how much of the blit was left when the writes
; were done:
;
;   rows 16-23:  full-width scale reference (always 20 words)
;   rows 32-39:  BLTPRI clear: polls left / 128 (long: the writes cost the
;                blitter one cycle in four, the blit far outlasts them)
;   rows 48-55:  BLTPRI set: polls left / 128 (shortest bar: the CPU gets no
;                cycle of an all-channel blit, so the writes end with it)
;   rows 72-79:  the BLTPRI-clear poll count as a 16-bit binary word
;   rows 88-95:  the BLTPRI-set poll count as a 16-bit binary word
;
; A model that never lets a posted write take a yielded blitter cycle
; stalls the write loop for the whole blit and collapses the first bar to
; the second. vAmiga is not a reference here: it has no 68020 bus model.
; Needs a 68020 or later (the golden runs it on the A1200 shape).
CUST   equ $dff000
BMP    equ $40000
WBUF   equ $48000
SRC    equ $50000
DST    equ $58000
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
        move.l #$01fc0000,(a1)+   ; FMODE: OCS fetch
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
        move.w #$83c0,$96(a6)     ; SET DMAEN|BPLEN|COPEN|BLTEN (BLTPRI clear)
        move.w #$ffff,$44(a6)     ; BLTAFWM
        move.w #$ffff,$46(a6)     ; BLTALWM

        ; scale reference bar
        moveq #20,d3
        moveq #16,d2
        bsr.w drawbar

        ; ---- BLTPRI clear ----
        bsr.w measure
        move.l d7,d4
        move.l d7,d3
        lsr.l  #7,d3
        addq.w #1,d3
        moveq  #32,d2
        bsr.w  drawbar
        move.w d4,d3
        moveq  #72,d2
        bsr.w  drawword

        ; ---- BLTPRI set ----
        move.w #$8400,$96(a6)     ; SET BLTPRI
        bsr.w measure
        move.w #$0400,$96(a6)     ; CLR BLTPRI
        move.l d7,d4
        move.l d7,d3
        lsr.l  #7,d3
        addq.w #1,d3
        moveq  #48,d2
        bsr.w  drawbar
        move.w d4,d3
        moveq  #88,d2
        bsr.w  drawword

.halt:  bra.s  .halt

; Start the all-channel blit, make 256 chip writes, then count polls until
; the blit is done. Returns the poll count in d7.
measure:
        move.w #$0fca,$40(a6)     ; BLTCON0 USEA|USEB|USEC|USED, LF=$CA
        clr.w  $42(a6)            ; BLTCON1
        move.l #SRC,$50(a6)       ; BLTAPT
        move.l #SRC+$2000,$4c(a6) ; BLTBPT
        move.l #DST,$48(a6)       ; BLTCPT
        move.l #DST,$54(a6)       ; BLTDPT
        clr.w  $64(a6)            ; BLTAMOD
        clr.w  $62(a6)            ; BLTBMOD
        clr.w  $60(a6)            ; BLTCMOD
        clr.w  $66(a6)            ; BLTDMOD
        lea    WBUF,a1
        moveq  #0,d0
        moveq  #0,d7
        move.w #255,d6
        move.w #8212,$58(a6)      ; BLTSIZE h=128 w=20
.wr:    move.w d0,(a1)
        dbra   d6,.wr
.poll:  addq.l #1,d7
        btst   #6,$02(a6)         ; DMACONR BBUSY
        bne.s  .poll
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

; Draw d3 as a 16-bit binary pattern, each bit 16 pixels wide (16 words),
; 8 rows tall, most significant bit left: d2 = first bitmap row.
drawword:
        lea    BMP,a0
        move.w d2,d0
        mulu   #40,d0
        adda.l d0,a0
        moveq  #8-1,d1
.row:   move.l a0,a2
        move.w d3,d5
        moveq  #16-1,d0
.bit:   add.w  d5,d5
        scs    d4
        ext.w  d4
        move.w d4,(a2)+
        dbra   d0,.bit
        lea    40(a0),a0
        dbra   d1,.row
        rts
