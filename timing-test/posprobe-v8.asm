; VPOSR/VHPOSR vertical-counter consistency (V8 against V7-V0).
;
; V8 (VPOSR bit 0) and V7-V0 (VHPOSR high byte) are one counter in Agnus:
; they step in the same colour clock, two positions into the line as
; VHPOSR reports it. If a model steps them at different colour clocks,
; a read of both inside that gap pairs the old V8 with the new low byte
; and the line number runs backwards: at line 255 -> 256 the pair reads as
; line 0, and at the frame wrap as line 256.
;
; The probe reads VPOSR:VHPOSR with one long read 131072 times and counts
; the samples whose line number is lower than the one before. A delay
; that changes on every iteration walks the sample point through every
; phase of the line, so the count does not depend on where the loop
; happens to lock against the beam.
;
;   rows 16-23:  full-width scale reference (always 20 words)
;   rows 32-39:  backward steps / 8 as a bar
;   rows 56-63:  backward steps as a 16-bit binary word
;
; With the two halves in step the count is the number of frames the
; sampling spans (one real wrap each) plus the tears of the long read
; itself: a 68000 reads the two words two colour clocks apart, so a sample
; whose word cycles straddle the counter step pairs the old V8 with the new
; low byte. A 3-cck gap between the registers adds about one more false
; step per 18 line-255 and frame-end crossings.
;
; On a Kickstart 1.3 A500 vAmiga 5 counts 109 and this model 110; without
; the tears of the long read the model counted 102.
CUST   equ $dff000
BMP    equ $40000
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
        move.w #$8380,$96(a6)     ; SET DMAEN|BPLEN|COPEN

        ; scale reference bar
        moveq #20,d3
        moveq #16,d2
        bsr.w drawbar

        lea    $04(a6),a0         ; VPOSR, with VHPOSR behind it
        moveq  #0,d7
        move.l (a0),d1
        and.l  #$0001ff00,d1
        moveq  #2-1,d5
.outer: move.w #$ffff,d6
.samp:  move.l (a0),d0            ; VPOSR:VHPOSR in one long read
        and.l  #$0001ff00,d0      ; V8-V0
        cmp.l  d1,d0
        bcc.s  .fwd
        addq.w #1,d7              ; the line number went backwards
.fwd:   move.l d0,d1
        move.w d6,d2
        and.w  #$001f,d2
        lsl.l  d2,d3              ; 8 + 2n clocks: walk the sample phase
        dbra   d6,.samp
        dbra   d5,.outer

        move.w d7,d4
        move.w d7,d3
        lsr.w  #3,d3
        moveq  #32,d2
        bsr.w  drawbar
        move.w d4,d3
        moveq  #56,d2
        bsr.w  drawword

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
