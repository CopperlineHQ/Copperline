; SPDX-License-Identifier: GPL-3.0-or-later
; AGA Lisa/Alice. At native SHRES pitch the blue field has a two-sample
; black slit wholly inside one colour clock. The red and green
; sprite stripes have a 2:1 vertical cadence. Both start on an odd line.
; DFF000 Writes and the Copper list are identical on an A1200 or A4000.
; The expected cadence and edge positions come from the AA registers spec.
; vAmiga does not provide a native 35 ns AGA reference frame for this test.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$096(a6)
        ; A fixed plane of zeroes makes the whole active picture COLOR00.
        lea     $40000,a0
        move.w  #16383,d0
.clear: clr.l   (a0)+
        dbra    d0,.clear
        lea     $60000,a0
        move.w  #$51c8,(a0)+     ; SH10 set, HSTART $190 folds to $90
        move.w  #$7100,(a0)+
        moveq   #7,d0
.red:   move.w  #$ffff,(a0)+
        clr.w   (a0)+
        clr.l   (a0)+            ; alternate solid and transparent rows
        dbra    d0,.red
        clr.l   (a0)+
        lea     $60200,a0
        move.w  #$5158,(a0)+     ; SH10 clear
        move.w  #$7100,(a0)+
        moveq   #15,d0
.green: clr.w   (a0)+
        move.w  #$ffff,(a0)+
        clr.l   (a0)+
        dbra    d0,.green
        clr.l   (a0)+
        lea     $61000,a0
        lea     list(pc),a1
        moveq   #(list_end-list)/4-1,d0
.copy:  move.l  (a1)+,(a0)+
        dbra    d0,.copy
        ; Use programmable PAL totals so the headless display preserves
        ; all 35 ns samples across the row as an actual SHRES monitor does.
        move.w  #$00e2,$1c0(a6)
        move.w  #$0138,$1c8(a6)
        move.w  #$00a0,$1dc(a6)  ; VARBEAMEN | PAL
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        move.w  #$1241,$100(a6)  ; ECSENA, one super-hires bitplane
        move.w  #$0041,$106(a6)  ; EXTBLKEN, low-resolution sprites
        move.w  #$0011,$10c(a6)
        move.w  #$8003,$1fc(a6)  ; SSCAN2, 64-bit bitplane fetch
        move.w  #$0360,$1c4(a6)  ; start at $60 + 3/8 CCK
        move.w  #$0560,$1c6(a6)  ; stop at $60 + 5/8 CCK
        move.w  #$000f,$180(a6)
        move.w  #$0f00,$1a2(a6)
        move.w  #$00f0,$1a4(a6)
        move.l  #$61000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$83a0,$096(a6)
.loop:  bra.s   .loop

list:   dc.l    $00e00004,$00e20000,$01200006,$01220000
        dc.l    $01240006,$01260200,$fffffffe
list_end:
