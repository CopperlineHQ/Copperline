; SPDX-License-Identifier: GPL-3.0-or-later
; Long write to a custom register pair: when each word lands.
;
; The custom chips sit on a 16-bit bus, so a 68000 long write to $DFFxxx
; is two word write cycles, two colour clocks apart, whichever instruction
; makes it. Each word reaches its register on its own cycle: the first
; word of the long lands where a word write starting at the same instant
; would land, and the second word two colour clocks later. The high word
; goes first, except to a predecrement destination, where the low word
; does.
;
; The Copper requests its interrupt at the same place on 108 lines and the
; CPU waits for each one in STOP. A stopped 68000 looks at its interrupt
; inputs every four clocks, counted from where STOP began (measured on an
; A500, see docs/internals/timing.md), so a handler can start one colour
; clock earlier or later than its neighbour's. Every handler therefore
; first reads the beam's horizontal count and waits out up to three colour
; clocks, and leaves that step at one distance from the beam. From there
; each handler is padded with NOPs so that its first register write cycle
; starts 16 clocks in and its common tail, which restores the background,
; 36 clocks in. With no bitplanes the
; raster shows COLOR00, as nine bands of 12 lines from line $50:
;
;   band 1, white:    move.w  d1,$180(a6)     word write, the reference
;   band 2, yellow:   move.l  d2,$180(a6)     COLOR00 is the FIRST word
;   band 3, orange:   move.l  d3,$dff180      COLOR00 is the FIRST word
;   band 4, red:      move.l  (a2),$dff180    COLOR00 is the FIRST word
;   band 5, magenta:  move.l  (a3),$180(a6)   COLOR00 is the FIRST word
;   band 6, violet:   movem.l d4,$180(a6)     COLOR00 is the FIRST word
;   band 7, cyan:     movem.l d5,$17e(a6)     COLOR00 is the SECOND word
;                                             (SPR7DATB takes the first)
;   band 8, green:    move.w  a5,$182(a6)     two word writes, COLOR00
;                     move.w  a5,$180(a6)     by the second
;   band 9, grey:     move.l  grey(pc),-(a1)  a1 = $DFF184: COLOR00 is the
;                                             high word, written SECOND
;                                             (COLOR01 takes the first)
;
;       move.w  Dn,d16(An)     12 clocks: prefetch, write, prefetch
;       move.l  Dn,d16(An)     16 clocks: prefetch, write, write, prefetch
;       move.l  Dn,(xxx).L     20 clocks: prefetch, prefetch, write, write,
;                                         prefetch
;       move.l  (An),(xxx).L   28 clocks: read, read, prefetch, write,
;                                         write, prefetch, prefetch
;       move.l  (An),d16(An)   24 clocks: read, read, prefetch, write,
;                                         write, prefetch
;       movem.l Dn,d16(An)     20 clocks: prefetch, prefetch, write, write,
;                                         prefetch
;       move.l  d16(PC),-(An)  24 clocks: prefetch, read, read, prefetch,
;                                         write (low), write (high)
;
; Expected: bands 1 to 6 share one left edge, bands 7 and 9 start two
; colour clocks (4 lo-res pixels) to their right and band 8 six colour
; clocks (12 lo-res pixels) to their right. All nine right edges line up.
; A model that applies both words of a long write after its second cycle
; draws the left edge of a first-word band under band 7 instead, and one
; that writes the high word of a predecrement long first draws band 9
; under band 1.
;
; vAmiga is a valid reference for this probe: it runs the two write cycles
; of every long write separately. vAmiga 4.4 and 5.0b1 (A500_OCS_1MB)
; render every edge in the column Copperline does; see
; timing-test/README.md for the comparison.
CUST          equ $dff000
LVOSuperState equ -150
CLIST         equ $60000
FIRST         equ $50             ; first sampled line
LINES         equ 12              ; lines per band

PHASE         equ 1               ; SYNC leaves at this count, modulo 4

; Leave at a fixed horizontal position: wait 0 to 3 colour clocks,
; whichever brings the count read here to PHASE modulo 4.
SYNC    macro
        moveq   #PHASE,d7
        sub.b   $07(a6),d7          ; VHPOSR, horizontal count
        and.w   #3,d7
        lsl.w   d7,d7               ; 6 + 2n clocks
        endm

; Restore the background 39 colour clocks later, acknowledge, return.
TAIL    macro
        moveq   #5,d7
.d\@:   dbra    d7,.d\@
        move.w  d0,$180(a6)
        move.w  #$0010,$9c(a6)
        rte
        endm

        move.w  296(a6),d7          ; ExecBase->AttnFlags, while the OS is alive
        jsr     LVOSuperState(a6)   ; supervisor mode for good
        move.w  #$2700,sr
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)
        move.w  #$7fff,$9c(a6)
        move.w  #$7fff,$96(a6)
        btst    #0,d7               ; 68010+: vectors at zero
        beq.s   .cpuok
        moveq   #0,d0
        dc.w    $4e7b,$0801         ; movec d0,vbr
.cpuok:
        ; Copper list: the background, then one COPER request per line.
        lea     CLIST,a0
        move.l  #$01000200,(a0)+    ; BPLCON0: no planes
        move.l  #$01800113,(a0)+    ; COLOR00: background
        move.w  #(FIRST<<8)|$21,d0  ; WAIT line, horizontal $20
        moveq   #9*LINES-1,d1
.cl:    move.w  d0,(a0)+
        move.w  #$fffe,(a0)+
        move.l  #$009c8010,(a0)+    ; INTREQ: SET|COPER
        add.w   #$0100,d0
        dbra    d1,.cl
        move.l  #$fffffffe,(a0)+
        move.l  #CLIST,$80(a6)
        move.w  d0,$88(a6)
        move.w  #$8280,$96(a6)      ; SET DMAEN|COPEN
        move.w  #$c010,$9a(a6)      ; SET INTEN|COPER

        move.w  #$0113,d0           ; background
        move.w  #$0fff,d1           ; band 1: white
        move.l  #$0ff00113,d2       ; band 2: yellow, then COLOR01
        move.l  #$0f800113,d3       ; band 3: orange, then COLOR01
        lea     red(pc),a2          ; band 4: red, then COLOR01
        lea     magenta(pc),a3      ; band 5: magenta, then COLOR01
        move.l  #$080f0113,d4       ; band 6: violet, then COLOR01
        move.l  #$000000ff,d5       ; band 7: SPR7DATB, then cyan
        movea.w #$00f0,a5           ; band 8: green
        lea     $184(a6),a1         ; band 9: predecrements onto COLOR00/01

frame:  move.w  #$2700,sr
        ; frame sync: wait for V8 to rise (line >= 256), then fall (wrap).
.f1:    move.l  $04(a6),d7
        btst    #16,d7
        beq.s   .f1
.f2:    move.l  $04(a6),d7
        btst    #16,d7
        bne.s   .f2
        move.w  #$0010,$9c(a6)      ; nothing pending from the last frame

        lea     handlers(pc),a4
.band:  move.w  (a4)+,d6
        bmi.s   frame
        lea     hword(pc),a0
        adda.w  d6,a0
        move.l  a0,$6c.w            ; level 3 autovector
        moveq   #LINES-1,d6
.line:  stop    #$2000              ; the request of the next line
        dbra    d6,.line
        bra.s   .band

handlers:
        dc.w    hword-hword
        dc.w    hmove-hword
        dc.w    habsreg-hword
        dc.w    habsmem-hword
        dc.w    hindmem-hword
        dc.w    hmovem-hword
        dc.w    hsecond-hword
        dc.w    hpair-hword
        dc.w    hpredec-hword
        dc.w    -1
red:    dc.l    $0f000113
magenta:
        dc.l    $0f0f0113
grey:   dc.l    $0aaa0113           ; band 9: grey, then COLOR01

hword:
        SYNC
        nop
        nop
        nop
        move.w  d1,$180(a6)         ; write from clock 16
        nop
        nop
        nop
        TAIL

hmove:
        SYNC
        nop
        nop
        nop
        move.l  d2,$180(a6)         ; writes from clocks 16 and 20
        nop
        nop
        TAIL

habsreg:
        SYNC
        nop
        nop
        move.l  d3,$dff180          ; writes from clocks 16 and 20
        nop
        nop
        TAIL

habsmem:
        SYNC
        nop
        move.l  (a2),$dff180        ; writes from clocks 16 and 20
        nop
        TAIL

hindmem:
        SYNC
        nop
        move.l  (a3),$180(a6)       ; writes from clocks 16 and 20
        nop
        nop
        TAIL

hmovem:
        SYNC
        nop
        nop
        movem.l d4,$180(a6)         ; writes from clocks 16 and 20
        nop
        nop
        TAIL

hsecond:
        SYNC
        nop
        nop
        movem.l d5,$17e(a6)         ; SPR7DATB from 16, COLOR00 from 20
        nop
        nop
        TAIL

hpair:
        SYNC
        nop
        nop
        nop
        move.w  a5,$182(a6)         ; COLOR01 from clock 16
        move.w  a5,$180(a6)         ; COLOR00 from clock 28
        TAIL

hpredec:
        SYNC
        move.l  grey(pc),-(a1)      ; COLOR01 from clock 16, COLOR00 from 20
        lea     $184(a6),a1         ; back to $DFF184
        nop
        TAIL
