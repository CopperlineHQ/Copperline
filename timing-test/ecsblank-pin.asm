; SPDX-License-Identifier: GPL-3.0-or-later
; ECS programmable horizontal blank, as the picture sees it.
;
; Agnus compares HBSTRT/HBSTOP and, with BEAMCON0.BLANKEN, puts the window
; on its CSY* pin. Denise blanks from that pin (its CBL* input) only while
; BPLCON3.EXTBLKEN is set and BPLCON0.ECSENA (ENBPLCN3) enables it. An OCS
; Denise has neither bit and keeps its fixed blank whatever Agnus does.
;
; The probe sets BLANKEN with HBSTRT=$60 and HBSTOP=$68, writes EXTBLKEN to
; BPLCON3 while ECSENA is still clear (the 8373 latches the bit regardless;
; ECSENA enables the feature, not the write), then sets ECSENA. The picture
; is a one-plane ruler of 8-pixel bars, yellow on dark blue.
;
; Expected, booted on an ECS Agnus:
;   with an ECS Denise: a black slit 16 lo-res pixels wide (colour clocks
;     $60-$67) through every line, borders included;
;   with an OCS Denise: the unbroken ruler.
;
; Sources: Commodore 8373 Denise specification (EXTBLKEN, ENBPLCN3) and
; WinUAE (check_vidsyncs, do_exthblankon_ecs). vAmiga 4.4 has no EXTBLKEN:
; it draws the unbroken ruler on A500_ECS_1MB and A500_PLUS_1MB alike. It
; matches the OCS-Denise render exactly (tools/vamiga-aga-compare.py) and
; is not a reference for the ECS-Denise one.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        ; One 40-byte ruler row, repeated on every line by the modulo.
        lea     $40000,a0
        moveq   #20-1,d0
.row:   move.w  #$ff00,(a0)+
        dbra    d0,.row
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
        clr.w   $104(a6)
        move.w  #-40,$108(a6)
        move.w  #-40,$10a(a6)
        move.w  #$0060,$1c4(a6)  ; HBSTRT
        move.w  #$0068,$1c6(a6)  ; HBSTOP
        move.w  #$0028,$1dc(a6)  ; BEAMCON0: PAL | BLANKEN
        move.w  #$0001,$106(a6)  ; EXTBLKEN, before ECSENA is set
        move.w  #$1201,$100(a6)  ; one lo-res plane, COLOR, ECSENA
        move.w  #$0008,$180(a6)
        move.w  #$0ff0,$182(a6)
        move.l  #$41000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$8380,$096(a6)
.loop:  bra.s   .loop

list:   dc.l    $00e00004,$00e20000
        dc.l    $fffffffe
list_end:
