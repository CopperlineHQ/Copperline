; DDFSTRT comparator-miss probe (AGA lo-res, 64-bit fetch).
;
; Agnus starts a line's bitplane fetch from a horizontal comparator: the DDF
; flop sets on the single colour clock where the counter equals DDFSTRT.  A
; DDFSTRT write that moves the match position behind the beam before the old
; position has fired therefore leaves the flop unset for the whole line - the
; counter never returns to that value before the horizontal wrap - and the
; line fetches nothing.  A write that lands after the flop has already set
; cannot un-start the run; only DDFSTOP ends it.
;
; Three bands over one repeated 20-word row (DDFSTRT $40 / DDFSTOP $C0 is
; five 64-pixel gulps; BPL1MOD = -40 repeats the row, so every fetched line
; renders identically):
;
;   lines $2C..$6B  control: no mid-line DDFSTRT write, row rendered.
;   lines $6C..$AB  miss:    DDFSTRT dropped to $20 at hpos ~$32, i.e. behind
;                            the beam and before the $40 match - the band is
;                            background, and DDFSTRT is restored to $40 at
;                            hpos $D0 for the next line.
;   lines $AC..$EB  rewrite: DDFSTRT rewritten with its own value at hpos
;                            ~$62, after the $40 match fired - the run is
;                            already going, so these lines match the control
;                            band exactly.
;   lines $EC..     control again.
;
; Restarting the fetch from the moved comparator instead of dropping the line
; renders the miss band as a shifted, gulp-truncated row rather than
; background, and in the multi-plane case hands only the planes whose lo-res
; slot number survives the truncated unit (BPL5 and BPL1, slots 6 and 7) an
; extra fetch, leaving those two bitplane pointers 8 bytes out of step with
; the rest for the remainder of the display.  That is the Microcosm (CD32)
; status-panel regression: its copper repoints seven bitplanes and drops
; DDFSTRT $2C -> $18 in one burst that overruns the line, so the new DDFSTRT
; commits at hpos ~$1E on the panel's first line.
;
; Cross-check basis: vAmiga 5.0b1 (its new A1200_2MB AGA setup) renders this
; probe with the same ink/paper mask as Copperline, pixel for pixel, once the
; two capture windows are aligned (vAmiga's raw frame starts two lines later,
; and expands COLOR00 $008 to $000072 where Copperline replicates the nibble
; to $000088): 0 of 202628 structural pixels differ, against 3.16% - exactly
; the 64-line miss band - for the restart-mid-unit behaviour this replaces.
; The band edges therefore come from an independent implementation, not from
; the comparator model alone.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000

MISS0  equ $6c                    ; first miss-band line
MISS1  equ $ac                    ; first rewrite-band line
RW1    equ $ec                    ; first trailing-control line

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        lea row(pc),a0
        lea BMP,a1
        moveq #20-1,d0
.copy: move.w (a0)+,(a1)+
        dbra d0,.copy

        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1: no scroll
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$0108ffd8,(a1)+   ; BPL1MOD = -40 bytes (repeat row)
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920040,(a1)+   ; DDFSTRT $40 (on the 32-cck gulp grid)
        move.l #$009400c0,(a1)+   ; DDFSTOP $C0 (five BPL64 gulps, 20 words)
        move.l #$008e2c81,(a1)+   ; standard DIWSTRT
        move.l #$00902cc1,(a1)+   ; standard DIWSTOP
        move.l #$01fc0003,(a1)+   ; FMODE = BPL64
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL

; Miss band: move the comparator behind the beam before it has fired, then
; put it back once the line's fetch window has passed.
        move.w #MISS0,d1
.miss: move.w d1,d2
        lsl.w #8,d2
        or.w #$0031,d2
        move.w d2,(a1)+           ; WAIT vp=d1, hp=$30
        move.w #$fffe,(a1)+
        move.l #$00920020,(a1)+   ; DDFSTRT $20 (already behind the beam)
        move.w d1,d2
        lsl.w #8,d2
        or.w #$00d1,d2
        move.w d2,(a1)+           ; WAIT vp=d1, hp=$D0
        move.w #$fffe,(a1)+
        move.l #$00920040,(a1)+   ; DDFSTRT $40 for the next line
        addq.w #1,d1
        cmp.w #MISS1,d1
        bne.s .miss

; Rewrite band: same value written back after the comparator has fired.
.rw:   move.w d1,d2
        lsl.w #8,d2
        or.w #$0061,d2
        move.w d2,(a1)+           ; WAIT vp=d1, hp=$60
        move.w #$fffe,(a1)+
        move.l #$00920040,(a1)+   ; DDFSTRT $40 again, mid-run
        addq.w #1,d1
        cmp.w #RW1,d1
        bne.s .rw

        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.loop: bra.s .loop

; One 20-word (320 lo-res pixel) row: solid edge bars, inward ticks, and
; asymmetric interior markers so a whole-gulp horizontal shift is obvious.
row:   dc.w $ffff,$8001,$0000,$0000,$ff00,$0000
        dc.w $0000,$0f0f,$0000,$0000,$00ff,$0000
        dc.w $0000,$3333,$0000,$0000,$c003,$0000
        dc.w $8001,$ffff
