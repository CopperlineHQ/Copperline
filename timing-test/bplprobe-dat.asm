; Manual BPL1DAT serialisation placement vs the DIW border (display DMA off).
;
; Writing BPL1DAT by hand (copper or CPU) loads Denise's bitplane holding
; register and displays one 16-pixel batch even with bitplane DMA disabled --
; the "chunky copper" display technique (Desire "Hamazing", Hexagon scene:
; the copper beam-races (COLORxx, BPL1DAT) pairs, one batch per 8 colour
; clocks). The held word does NOT start shifting at the write position:
; Denise's serialiser parallel-loads on its free-running 16-lores-pixel word
; cadence (the same load strobe DMA-fetched words use), so the batch snaps to
; the next word-grid slot after the write lands, and the DIW comparators clip
; it there like any fetched pixel. Consequences this probe renders:
;   - bars from WAIT positions 4 ccks apart can land in the SAME grid slot
;     (adjacent bands paint identical bars);
;   - a batch whose grid slot starts left of DIW HSTART is border-clipped
;     (a raced per-line stream gets a straight window-edge clip -- the
;     Hamazing Hexagon field's left edge, this probe's regression class);
;   - re-arming before the next load strobe just replaces the held word:
;     back-to-back MOVEs produce ONE batch, not two.
;
; Display: COLOR00 dark blue (border and window interior -- COLOR00 is also
; the border colour, so no window edge line is visible; the bar clipping
; itself encodes the edge), COLOR01 white bars from BPL1DAT=$FFFF.
; Bitplane DMA stays off the whole time; only copper (and the CPU spin loop)
; touch the bus, so every line of a band gets the identical WAIT release and
; write landing.
;
; Bands of 12 lines, top to bottom (all arms MOVE #value,BPL1DAT):
;   v48   blank ruler  $0000 @ WAIT h$51
;   v60   bar          $FFFF @ WAIT h$29  } batch grid slots walk right in
;   v72   bar                @ WAIT h$2d  } 8-cck steps while the WAITs step
;   v84   bar                @ WAIT h$31  } 4 ccks: bands pair up, and slots
;   v96   bar                @ WAIT h$35  } left of the window edge clip to
;   v108  bar                @ WAIT h$37  } nothing or a tail ($37 steps
;   v120  bar                @ WAIT h$39  } 2 ccks to pin the latch-to-load
;   v132  bar                @ WAIT h$3d  } offset)
;   v144  bar                @ WAIT h$41
;   v156  bar                @ WAIT h$51
;   v168  double write $FFFF,$FFFF @ h$51 (re-arm before the load strobe:
;                                          one 16-lores-px batch, not two)
;   v180  bit order    $F0F0 @ h$51      (MSB first: 4-lores-px comb)
;   v192  scroll       BPLCON1=$44, $FFFF @ h$51 (PF1H=4: batch shifts 4
;                                          lores px right of its grid slot)
;   v204  BPU cut      arm, BPLCON0=$0200 two copper slots later (a plane-
;                                          count drop just after the batch
;                                          does not truncate it)
;   v216  colour flip  arm, COLOR01=red next copper slot (the palette write
;                                          lands before the batch shifts
;                                          out: the whole bar is red)
;   v228  hires bar    BPLCON0=$9200, $FFFF @ h$51 } 16-hires-px batches on
;   v240  hires bar                         @ h$53 } the hires word cadence
;   v252  guard        BPLCON0=$1200, $0000 @ h$51 (4 lines)
;
; Cross-check: vAmiga is a valid reference for this OCS Denise behaviour
; (tools/vamiga-ref.sh, A500_OCS_1MB); its serialiser model reproduces the
; grid-snapped batches and DIW clipping that real hardware shows on the
; Hamazing Hexagon scene.
CUST   equ $dff000

        lea CUST,a6
        move.w #$7fff,$9a(a6)    ; INTENA clear
        move.w #$7fff,$9c(a6)    ; INTREQ clear
        move.w #$7fff,$96(a6)    ; DMACON clear
        lea clist(pc),a0
        move.l a0,$80(a6)        ; COP1LC
        move.w d0,$88(a6)        ; COPJMP1
        move.w #$8280,$96(a6)    ; DMAEN|COPEN (no bitplane, sprite, disk DMA)
.l:     bra.s .l

; One 12-line band: per line WAIT(v,\2) full-compare, then MOVE #\3,BPL1DAT.
BAND    MACRO                    ; \1 = start vpos, \2 = wait hpos, \3 = value
        REPT 12
        dc.w ((\1+REPTN)<<8)|\2,$fffe
        dc.w $0110,\3
        ENDR
        ENDM

        cnop 0,4
clist:
        dc.w $0100,$1200         ; BPLCON0: 1 plane, COLOR ON, lores
        dc.w $0102,$0000         ; BPLCON1
        dc.w $0104,$0000         ; BPLCON2
        dc.w $0180,$0024         ; COLOR00 dark blue
        dc.w $0182,$0fff         ; COLOR01 white (bars)
        dc.w $008e,$2c81         ; DIWSTRT: v44 h$81
        dc.w $0090,$2cc1         ; DIWSTOP: v300 h$1C1

        BAND $30,$51,$0000       ; ruler
        BAND $3c,$29,$ffff
        BAND $48,$2d,$ffff
        BAND $54,$31,$ffff
        BAND $60,$35,$ffff
        BAND $6c,$37,$ffff
        BAND $78,$39,$ffff
        BAND $84,$3d,$ffff
        BAND $90,$41,$ffff
        BAND $9c,$51,$ffff

        ; double write: two back-to-back MOVEs per line
        REPT 12
        dc.w (($a8+REPTN)<<8)|$51,$fffe
        dc.w $0110,$ffff
        dc.w $0110,$ffff
        ENDR

        BAND $b4,$51,$f0f0       ; bit order

        dc.w ($bf<<8)|$95,$fffe  ; after the $b4 band's last display line
        dc.w $0102,$0044         ; BPLCON1: PF1H=4 for the scroll band
        BAND $c0,$51,$ffff

        dc.w ($cb<<8)|$95,$fffe
        dc.w $0102,$0000         ; BPLCON1 back to 0

        ; BPU cut: arm, one interposed MOVE, then BPLCON0 to 0 planes;
        ; restore BPLCON0 after the batch (invisible: both sides show
        ; COLOR00 inside the window).
        REPT 12
        dc.w (($cc+REPTN)<<8)|$51,$fffe
        dc.w $0110,$ffff
        dc.w $01fe,$0000         ; no-op MOVE: one copper slot of spacing
        dc.w $0100,$0200
        dc.w (($cc+REPTN)<<8)|$71,$fffe
        dc.w $0100,$1200
        ENDR

        ; colour flip: arm, then COLOR01 white->red in the next copper slot;
        ; restore after the batch (invisible: nothing shows COLOR01 there).
        REPT 12
        dc.w (($d8+REPTN)<<8)|$51,$fffe
        dc.w $0110,$ffff
        dc.w $0182,$0f00
        dc.w (($d8+REPTN)<<8)|$71,$fffe
        dc.w $0182,$0fff
        ENDR

        dc.w ($e3<<8)|$95,$fffe
        dc.w $0100,$9200         ; BPLCON0: hires, 1 plane
        BAND $e4,$51,$ffff
        BAND $f0,$53,$ffff

        dc.w ($fb<<8)|$95,$fffe
        dc.w $0100,$1200         ; BPLCON0 back to lores
        REPT 4                   ; guard (v252..255)
        dc.w (($fc+REPTN)<<8)|$51,$fffe
        dc.w $0110,$0000
        ENDR

        dc.w $ffff,$fffe
