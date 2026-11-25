; SPDX-License-Identifier: GPL-3.0-or-later
; Denise's display gate under BRDRBLNK: DIW flip-flop AND first BPL1DAT.
;
; DIWSTRT alone does not open Denise's display window. The window needs the
; horizontal DIW flip-flop set AND the rasterline's first BPL1DAT load;
; until that load the output is border. BPLCON3.BRDRBLNK (with ECSENA)
; paints the border pure black instead of COLOR00, which makes the gate
; visible. Sprites are clipped by the same gate.
;
; Hardware evidence (vAmigaTS, real-machine photographs):
;   Denise/Registers/BPLCON3/brdrblnk2 (A1200): BPU=0 lines are border
;     across the full width, black under BRDRBLNK whatever DIW says;
;     BRDRBLNK takes effect mid-line at the pixel it is written.
;   Agnus/DDF/AGADDF (A1200): black left of the bitplane data, no COLOR00
;     between DIWSTRT and the first fetched word.
;   Denise/Sprites/clip/diwclip1..8 (A500 OCS Denise, A500+, A1200): a
;     sprite left of the first data pixel stays hidden while DIWSTRT is
;     left of the data -- the staircase has a knee at the data start.
;
; Machine: ECS Agnus + ECS Denise (BRDRBLNK needs the 8373). DIW $2C81 to
; $2CC1, one lo-res plane of $FF00 bars (yellow / grey COLOR00 $444),
; BRDRBLNK set except on the ruler lines. Bands, top to bottom:
;
;   $2C-$3F  DDFSTRT $38: black | bars from $81 to $1C1 | black.
;   $40-$4F  DDFSTRT $50, DDFSTOP $A0: black up to the first data pixel
;            at $B1 (not grey from $81), bars to $161, then grey (window
;            still open after the data) to $1C1, then black.
;   $50      plane-less Copper ruler, BRDRBLNK clear: 40 COLOR00 moves,
;            white first, yellow/red, green last.
;   $51-$5F  plane-less, BRDRBLNK set: black across the whole line.
;   $60      the ruler again with BRDRBLNK written at stripe 20 and cleared
;            at stripe 32: black from the first write to the second.
;   $61-$6F  plane-less, BRDRBLNK clear: COLOR00 grey across the line.
;   $70-$7F  DDFSTRT $50 again, with two sprites: sprite 2 (magenta) at
;            HSTART $90, wholly between DIWSTRT and the data -- hidden;
;            sprite 0 (green) at HSTART $A8, straddling the first data
;            pixel -- shown only from $B1.
;   $80-$8F  DDFSTRT $38: both sprites shown in full.
;   $90-$9F  plane-less, BRDRBLNK set: black, sprites hidden.
;   $A0-$AF  DDFSTRT $3C, off the lo-res fetch grid: BPL1DAT is written at
;            $43 and opens the gate there (8 lo-res pixels right of $81),
;            but the picture waits for the $40 reload slot and starts at
;            $91, so the line reads black | grey strip $89-$91 | bars
;            (vAmigaTS Agnus/DDF/AGADDF agaddf3 A500+ and A1200 photos).
;   $B0-     DDFSTRT $38 bars to the bottom of the window.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        ; Bitplane memory: $FF00 bars everywhere the pointer can walk.
        lea     $40000,a0
        move.w  #$2000-1,d0
.fill:  move.w  #$ff00,(a0)+
        dbra    d0,.fill
        ; Sprite 0 at $44000, sprite 2 at $44100, a null sprite at $44200.
        lea     $44000,a0
        move.l  #$7054a000,(a0)+
        bsr     sprdata
        lea     $44100,a0
        move.l  #$7048a000,(a0)+
        bsr     sprdata
        lea     $44200,a0
        clr.l   (a0)+
        clr.l   (a0)+
        ; Copper list at $45000, clear of the bitplane walk.
        lea     $45000,a0
        lea     list(pc),a1
        move.w  #(list_end-list)/2-1,d0
.copy:  move.w  (a1)+,(a0)+
        dbra    d0,.copy
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        clr.w   $102(a6)
        move.w  #$0024,$104(a6)  ; sprites in front of the playfield
        clr.w   $108(a6)
        clr.w   $10a(a6)
        move.w  #$1201,$100(a6)  ; one lo-res plane, COLOR, ECSENA
        move.w  #$0020,$106(a6)  ; BRDRBLNK
        move.w  #$0444,$180(a6)
        move.w  #$0ff0,$182(a6)
        move.w  #$00f0,$1a2(a6)  ; COLOR17: sprite 0/1 colour 1
        move.w  #$0f0f,$1aa(a6)  ; COLOR21: sprite 2/3 colour 1
        move.l  #$45000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$83a0,$096(a6)  ; DMAEN BPLEN COPEN SPREN
.loop:  bra.s   .loop

; 48 lines of solid colour-1 data, then the end-of-sprite words.
sprdata:
        moveq   #48-1,d0
.line:  move.l  #$ffff0000,(a0)+
        dbra    d0,.line
        clr.l   (a0)+
        rts

; One plane-less ruler line: 40 back-to-back COLOR00 moves from h=$30.
RULER   macro
        dc.w    $180,$0fff
        rept    19
        dc.w    $180,$0ff0,$180,$0f00
        endr
        dc.w    $180,$00f0
        dc.w    $180,$0444
        endm

list:   dc.w    $00e0,$0004,$00e2,$0000
        dc.w    $0120,$0004,$0122,$4000     ; SPR0PT
        dc.w    $0124,$0004,$0126,$4200     ; SPR1PT null
        dc.w    $0128,$0004,$012a,$4100     ; SPR2PT
        dc.w    $012c,$0004,$012e,$4200
        dc.w    $0130,$0004,$0132,$4200
        dc.w    $0134,$0004,$0136,$4200
        dc.w    $0138,$0004,$013a,$4200
        dc.w    $013c,$0004,$013e,$4200
        dc.w    $0092,$0038,$0094,$00d0
        dc.w    $0100,$1201,$0106,$0020
        ; $40: late DDF band.
        dc.w    $4007,$fffe
        dc.w    $0092,$0050,$0094,$00a0
        ; $50: plane-less ruler, BRDRBLNK clear.
        dc.w    $5007,$fffe
        dc.w    $0092,$0038,$0094,$00d0
        dc.w    $0100,$0201,$0106,$0000
        dc.w    $5031,$fffe
        RULER
        ; $51-$5F: plane-less, blanked.
        dc.w    $5107,$fffe
        dc.w    $0106,$0020
        ; $60: ruler with a mid-line BRDRBLNK window (stripes 20..31).
        dc.w    $6007,$fffe
        dc.w    $0106,$0000
        dc.w    $6031,$fffe
        dc.w    $180,$0fff
        rept    9
        dc.w    $180,$0ff0,$180,$0f00
        endr
        dc.w    $180,$0ff0
        dc.w    $106,$0020                  ; stripe 20
        rept    5
        dc.w    $180,$0ff0,$180,$0f00
        endr
        dc.w    $180,$0ff0
        dc.w    $106,$0000                  ; stripe 32
        rept    3
        dc.w    $180,$0ff0,$180,$0f00
        endr
        dc.w    $180,$0ff0
        dc.w    $180,$00f0
        dc.w    $180,$0444
        ; $61-$6F: plane-less, unblanked: COLOR00 across the line.
        ; $70: late DDF with sprites.
        dc.w    $7007,$fffe
        dc.w    $0092,$0050,$0094,$00a0
        dc.w    $0100,$1201,$0106,$0020
        ; $80: standard DDF with sprites.
        dc.w    $8007,$fffe
        dc.w    $0092,$0038,$0094,$00d0
        ; $90: plane-less, blanked, sprites present.
        dc.w    $9007,$fffe
        dc.w    $0100,$0201
        ; $A0: off-grid lo-res DDFSTRT $3C.
        dc.w    $a007,$fffe
        dc.w    $0100,$1201
        dc.w    $0092,$003c
        ; $B0: standard bars to the bottom.
        dc.w    $b007,$fffe
        dc.w    $0092,$0038
        dc.w    $ffff,$fffe
list_end:
