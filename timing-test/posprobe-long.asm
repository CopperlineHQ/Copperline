; Long read of a custom register pair: when the first word is sampled.
;
; The custom chips sit on a 16-bit bus, so a 68000 MOVE.L from $DFFxxx is
; two word cycles, two colour clocks apart. Each word is returned by its
; own cycle: the first word of the long read is the value a word read
; starting at the same position would return, and the second word is two
; colour clocks newer.
;
; The probe reads VHPOSR with a word read and then, in the next
; instruction, with a long read (VHPOSR is the first word; the second is
; DSKDATR and is ignored). All DMA is off while it samples, so nothing but
; the memory refresh slots competes for the bus:
;
;       move.w (a0),d0      ; 8 clocks: read, prefetch
;       move.l (a0),d1      ; 12 clocks: read, read, prefetch
;
; The first word cycle of the MOVE.L starts 8 clocks = 4 colour clocks
; after the word cycle of the MOVE.W, so the horizontal counts of the two
; samples differ by 4. A model that returns both words of the long read
; from the end of its second cycle reports 6.
;
; 320 samples are taken with a delay that changes on every iteration, so
; the pair walks through the line. Each distance from 2 to 9 colour clocks
; has a bar whose length is its number of samples / 16 (an empty bucket
; keeps a one-word stub so the rows stay visible):
;
;   rows  16-23:   full-width scale reference (always 20 words)
;   rows  32-39:   distance 2
;   rows  44-51:   distance 3
;   rows  56-63:   distance 4      <- the long bar on hardware
;   rows  68-75:   distance 5
;   rows  80-87:   distance 6
;   rows  92-99:   distance 7
;   rows 104-111:  distance 8
;   rows 116-123:  distance 9
;
; Samples that straddle the end of a line or wait behind a refresh slot
; land in other buckets; they are the few missing from the long bar.
CUST   equ $dff000
BMP    equ $40000
CNT    equ $50000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)     ; all DMA off while sampling
        lea BMP,a0
        move.w #10240-1,d0
.cz:    clr.w (a0)+
        dbra d0,.cz
        lea CNT,a1
        moveq #16-1,d0
.cc:    clr.w (a1)+
        dbra d0,.cc

        lea    $06(a6),a0         ; VHPOSR, with DSKDATR behind it
        lea    CNT,a1
        move.w #320-1,d6
.samp:  move.w (a0),d0            ; one word cycle
        move.l (a0),d1            ; two word cycles
        swap   d1                 ; the first word of the long read
        sub.b  d0,d1              ; horizontal distance in colour clocks
        and.w  #$00ff,d1
        cmp.w  #15,d1
        bls.s  .ok
        moveq  #15,d1
.ok:    add.w  d1,d1
        addq.w #1,0(a1,d1.w)
        move.w d6,d2
        and.w  #$001f,d2
        lsl.l  d2,d3              ; 8 + 2n clocks: walk the sample phase
        dbra   d6,.samp

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
        move.w #$8380,$96(a6)     ; SET DMAEN|BPLEN|COPEN

        ; scale reference bar
        moveq #20,d3
        moveq #16,d2
        bsr.w drawbar

        lea    CNT+4,a3           ; distance 2 first
        moveq  #32,d7
        moveq  #8-1,d5
.bars:  move.w (a3)+,d3
        lsr.w  #4,d3
        move.w d7,d2
        bsr.w  drawbar
        add.w  #12,d7
        dbra   d5,.bars

.halt:  bra.s  .halt

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
