; Wait-loop skip differential guest (src/emulator/wait_skip.rs tests).
;
; Position independent: loaded into fast RAM (assembled for accelerator
; RAM at $08000000) and entered in supervisor mode at IPL 7 with A7
; pointing at the end of the image. Three interrupt
; sources set bits of a flag word in fast RAM, and the main loop polls it:
;
;   bit 0  VERTB (level 3), which also starts a blit every frame
;   bit 1  BLIT  (level 3), when that blit finishes
;   bit 2  CIA-A timer A (level 2 PORTS), about every millisecond
;
; D7 holds the CACR value to start with (zero leaves the caches off, and
; skips the MOVEC on a CPU without one).
;
; The poll at `wait` is the pure loop the skip retires: a fast-RAM read and
; a branch, no write. Each wake clears the flag and counts itself, so a
; skipped run that wakes on a different instruction ends in a different
; state.
;
; Assemble: vasmm68k_mot -Fbin -m68040 -no-opt -o guest.bin guest.asm

        org     $08000000

start:
        tst.l   d7
        beq.s   .nocache
        movec   d7,cacr
.nocache:
        lea     data(pc),a4
        lea     $dff000,a5
        lea     $bfe001,a6
        lea     level2(pc),a0
        move.l  a0,$68.w
        lea     level3(pc),a0
        move.l  a0,$6c.w
        move.w  #$7fff,$9a(a5)          ; INTENA: all off
        move.w  #$7fff,$9c(a5)          ; INTREQ: all clear
        move.b  #$7f,$d00(a6)           ; CIA-A ICR: all off
        move.b  #$c5,$400(a6)           ; TALO
        move.b  #$02,$500(a6)           ; TAHI: 709 E ticks
        move.b  #$81,$d00(a6)           ; ICR: timer A on
        move.b  #$11,$e00(a6)           ; CRA: load and start, continuous
        move.w  #$8240,$96(a5)          ; DMACON: DMAEN | BLTEN
        move.w  #$c068,$9a(a5)          ; INTENA: INTEN | BLIT | VERTB | PORTS
        move.w  #$2000,sr

wait:   tst.w   (a4)
        beq.s   wait
        move.w  (a4),d0
        clr.w   (a4)
        addq.l  #1,4(a4)                ; wakes
        or.w    d0,2(a4)                ; every bit seen
        bra.s   wait

level3: move.l  d0,-(a7)
        move.w  $1e(a5),d0              ; INTREQR
        and.w   $1c(a5),d0              ; INTENAR
        btst    #5,d0
        beq.s   .blit
        move.w  #$0020,$9c(a5)          ; acknowledge VERTB
        or.w    #1,(a4)
        addq.l  #1,8(a4)                ; frames
        move.l  #$00010000,$54(a5)      ; BLTDPT
        move.w  #0,$66(a5)              ; BLTDMOD
        move.l  #$01000000,$40(a5)      ; BLTCON0/1: D only, clear
        move.w  #(64<<6)|20,$58(a5)     ; BLTSIZE: 64 lines of 20 words
.blit:  btst    #6,d0
        beq.s   .done
        move.w  #$0040,$9c(a5)          ; acknowledge BLIT
        or.w    #2,(a4)
.done:  move.l  (a7)+,d0
        rte

level2: move.l  d0,-(a7)
        move.b  $d00(a6),d0             ; CIA-A ICR: read acknowledges
        move.w  #$0008,$9c(a5)          ; acknowledge PORTS
        or.w    #4,(a4)
        addq.l  #1,12(a4)               ; timer ticks
        move.l  (a7)+,d0
        rte

        cnop    0,4
data:   dc.w    0                       ; flag
        dc.w    0                       ; every bit seen
        dc.l    0                       ; wakes
        dc.l    0                       ; frames
        dc.l    0                       ; timer ticks
        ds.b    1024
stack:
