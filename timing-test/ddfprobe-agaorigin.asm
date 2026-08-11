; AGA wide-FMODE absolute gulp-grid origin probe (lo-res, 64-bit fetch).
;
; DDFSTRT $18 / DDFSTOP $B8 fetches six 64-pixel gulps per plane.  With
; the standard DIW $81..$1C1, Lisa's absolute reload grid puts the whole
; first gulp left of the display-window edge: source pixels 64..383 fill
; the 320-pixel window exactly.  The wide-fetch display origin remains
; linear below the standard fetch slots; clamping it to the DDF hard start
; moves the picture 48 pixels right, exposes most of the checkerboard first
; gulp at the left edge, and crops the right marker.
;
; The first four words are the hidden checkerboard gulp.  The following 20
; words have solid edge bars and sparse ticks, making both the hidden seam
; and the restored right edge obvious.  BPL1MOD=-48 repeats the same 24-word
; row and also pins the six-gulp word count.
;
; Cross-check basis: an FS-UAE/WinUAE-core A1200 capture of the same lo-res
; BPL64 $18/$B8 + standard-DIW constellation hides the full first gulp and
; fills the window flush at both edges (2026-08-11).  vAmiga is OCS/ECS-only
; and cannot arbitrate AGA.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        lea BMP,a0
        move.w #$aaaa,(a0)+       ; hidden 64-pixel seam gulp
        move.w #$5555,(a0)+
        move.w #$f0f0,(a0)+
        move.w #$0f0f,(a0)+
        move.w #$ffff,(a0)+       ; visible left edge bar
        move.w #$8001,(a0)+       ; inward-facing edge ticks
        move.w #16-1,d0
.zero: clr.w (a0)+                ; visible centre
        dbra d0,.zero
        move.w #$8001,(a0)+       ; inward-facing edge ticks
        move.w #$ffff,(a0)+       ; visible right edge bar

        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1: no scroll
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$0108ffd0,(a1)+   ; BPL1MOD = -48 bytes (repeat row)
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920018,(a1)+   ; DDFSTRT $18 (gulp slot 0)
        move.l #$009400b8,(a1)+   ; DDFSTOP $B8 (six BPL64 gulps)
        move.l #$008e2c81,(a1)+   ; standard DIWSTRT
        move.l #$00902cc1,(a1)+   ; standard DIWSTOP
        move.l #$01fc0003,(a1)+   ; FMODE = BPL64
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.loop: bra.s .loop
