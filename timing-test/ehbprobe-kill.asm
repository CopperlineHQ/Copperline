; SPDX-License-Identifier: GPL-3.0-or-later
; BPLCON2.KILLEHB across Denise generations.
;
; Six lo-res planes without HAM or dual playfield are extra-half-brite:
; plane 6 halves the colour the other five select. ECS Denise (8373) and
; Lisa add BPLCON2 bit 9, KILLEHB, which turns that off; it needs no
; BPLCON0.ECSENA. OCS Denise (8362) has no such bit and ignores the write.
;
; The probe sets KILLEHB and shows a ruler of 8-pixel bars (COLOR01 orange
; on COLOR00 grey) with plane 6 clear on the left half of the line and set
; on the right half.
;
; Expected:
;   OCS Denise: the right half at half intensity (extra-half-brite);
;   ECS Denise: both halves alike (plane 6 ignored, colours 0-31 only).
;
; Sources: Commodore 8373 Denise specification (BPLCON2 KILLEHB) and
; WinUAE (setbplmode: the bit is tested only for an ECS Denise). vAmiga
; 4.4 has no KILLEHB: it halves the right half on A500_OCS_1MB and
; A500_PLUS_1MB alike. It matches the OCS-Denise render exactly
; (tools/vamiga-aga-compare.py) and is not a reference for the ECS-Denise
; one.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        ; Six 40-byte plane rows, 64 bytes apart, repeated by the modulo.
        lea     $40000,a0
        move.w  #6*64/4-1,d0
.clear: clr.l   (a0)+
        dbra    d0,.clear
        lea     $40000,a0        ; plane 1: the ruler
        moveq   #20-1,d0
.row:   move.w  #$ff00,(a0)+
        dbra    d0,.row
        lea     $40000+5*64+20,a0 ; plane 6: set on the right half
        moveq   #10-1,d0
.half:  move.w  #$ffff,(a0)+
        dbra    d0,.half
        lea     $41000,a0
        lea     list(pc),a1
        moveq   #(list_end-list)/4-1,d0
.copy:  move.l  (a1)+,(a0)+
        dbra    d0,.copy
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        clr.w   $102(a6)
        move.w  #$0200,$104(a6)  ; KILLEHB
        move.w  #-40,$108(a6)
        move.w  #-40,$10a(a6)
        move.w  #$6200,$100(a6)  ; six lo-res planes, COLOR
        move.w  #$0888,$180(a6)
        move.w  #$0f80,$182(a6)
        move.l  #$41000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$8380,$096(a6)
.loop:  bra.s   .loop

list:   dc.l    $00e00004,$00e20000,$00e40004,$00e60040
        dc.l    $00e80004,$00ea0080,$00ec0004,$00ee00c0
        dc.l    $00f00004,$00f20100,$00f40004,$00f60140
        dc.l    $fffffffe
list_end:
