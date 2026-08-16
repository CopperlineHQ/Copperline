; Stable single-DIWSTRT clip test (red border for contrast).
; Copper reloads BPL1PT and all display regs every frame. Content solid white,
; DDFSTRT $30 (fetch origin ~$71). DIWSTRT H=$C8 with DIWSTOP H=$E8: the
; decoded HSTOP $1E8 lies beyond the $1C7 wrap of Denise's horizontal
; counter, so the window flip-flop never clears once set and carries open
; across every line. The DIWSTRT match on the already-open flip-flop is a
; no-op, so the picture shows from its fetch origin: white from ~x30 to the
; right edge, red only over the pre-fetch columns (vAmiga-verified; FS-UAE
; agrees). A renderer that treats DIW as a start/stop range instead of the
; comparator flip-flop clips the left ~1/4 of the screen to red. This is the
; golden probe for the carried-open window (Chambers of Shaolin's Grandslam
; intro, DIWSTRT $C0 / DIWSTOP $1D8, hides its logo's left edge otherwise).
CUST   equ $dff000
SCREEN equ $40000
FILLW  equ 14336
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea SCREEN,a0
        move.w #FILLW-1,d0
.f:     move.w #$ffff,(a0)+
        dbra d0,.f
        lea clist(pc),a0
        move.l a0,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)
.l:     bra.s .l
        cnop 0,4
clist:
        dc.w $0100,$1200
        dc.w $0102,$0000
        dc.w $0108,$0000
        dc.w $010a,$0000
        dc.w $0180,$0f00           ; COLOR00 = RED
        dc.w $0182,$0fff           ; COLOR01 = white
        dc.w $0092,$0030           ; DDFSTRT $30
        dc.w $0094,$00e0           ; DDFSTOP $E0 (content extends right)
        dc.w $008e,$2cc8           ; DIWSTRT H=$C8 (should clip left ~1/4)
        dc.w $0090,$2ce8           ; DIWSTOP H=$E8 (wide right)
        dc.w $00e0,$0004
        dc.w $00e2,$0000
        dc.w $ffff,$fffe
