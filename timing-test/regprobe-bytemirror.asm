; CPU byte-write custom-register mirror probe (border-colour stripes).
;
; A 68000 byte write to a custom register drives the SAME value onto both
; halves of the data bus, so the register latches the mirrored word
; (val<<8 | val). With display DMA fully off the whole raster shows
; COLOR00; once per frame the CPU rewrites it at four line boundaries:
;
;   lines $50..$6F: move.w #$0FFF -> white   (reference stripe)
;   lines $70..$8F: move.b #$0F to $DFF180 (even byte): mirrored latch
;                   -> $0F0F -> magenta. A wrong zero-extended model shows
;                   $0F00 -> red.
;   lines $90..$AF: move.b #$0F to $DFF181 (odd byte): mirrored latch
;                   -> magenta again; a wrong model shows $000F -> blue.
;   line  $B0:      back to the dark background.
;
; The stripe boundaries also pin the coarse CPU write landing against the
; VHPOSR line sync.
CUST   equ $dff000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        move.w #$0113,$180(a6)    ; background

frame:
        ; frame sync: wait for V8 to rise (line >= 256), then fall (wrap).
.f1:    move.l $04(a6),d0
        btst   #16,d0
        beq.s  .f1
.f2:    move.l $04(a6),d0
        btst   #16,d0
        bne.s  .f2

        move.w #$50,d2
        bsr.s  lwait
        move.w #$0fff,$180(a6)    ; word reference stripe
        move.w #$70,d2
        bsr.s  lwait
        move.b #$0f,$180(a6)      ; even (high) byte -> mirrored word
        move.w #$90,d2
        bsr.s  lwait
        move.b #$0f,$181(a6)      ; odd (low) byte -> mirrored word
        move.w #$b0,d2
        bsr.s  lwait
        move.w #$0113,$180(a6)    ; background below the stripes
        bra.s  frame

; Spin until VHPOSR's V7-0 reads d2.
lwait:  move.w $06(a6),d0
        lsr.w  #8,d0
        cmp.b  d2,d0
        bne.s  lwait
        rts
