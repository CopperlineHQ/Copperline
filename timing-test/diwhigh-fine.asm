; SPDX-License-Identifier: GPL-3.0-or-later
; AGA Lisa: the fine horizontal bits of DIWHIGH.
;
; Lisa's display-window comparators have two bits below the low-resolution
; position DIWSTRT/DIWSTOP name: H1 (a 70 ns step) and H0 (a 35 ns step),
; in DIWHIGH bits 4-3 for the start and bits 12-11 for the stop. They move
; the window edge only; the fetched picture stays where it is. ECS Denise
; has no such bits.
;
; A plane of ones fills the open window with yellow over a dark blue
; border. The Copper rewrites DIWHIGH for twelve 16-line bands:
;   lines $40-$7F  start fine 0, 1, 2, 3 (stop fine 0)
;   lines $80-$BF  stop fine 0, 1, 2, 3  (start fine 0)
;   lines $C0-$FF  start and stop fine both 0, 1, 2, 3
; Expected, at the native 35 ns column pitch: each step of the fine value
; moves that band's left (start) or right (stop) edge one column to the
; right of the band before it, and the other edge stays put.
;
; The scan uses programmable PAL totals so the headless frame keeps every
; 35 ns column, as lisa-scanblank does. Source: Commodore Lisa
; specification 391227 (DIWHIGH: H1 is a 70 ns increment and H0 a 35 ns
; increment) and WinUAE (calchdiw). vAmiga has no native 35 ns AGA frame
; to compare with.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        lea     $40000,a0
        moveq   #-1,d1
        move.w  #16383,d0
.fill:  move.l  d1,(a0)+
        dbra    d0,.fill
        lea     $61000,a0
        lea     list(pc),a1
        moveq   #(list_end-list)/4-1,d0
.copy:  move.l  (a1)+,(a0)+
        dbra    d0,.copy
        move.w  #$00e2,$1c0(a6)  ; HTOTAL
        move.w  #$0138,$1c8(a6)  ; VTOTAL
        move.w  #$00a0,$1dc(a6)  ; VARBEAMEN | PAL
        move.w  #$2c81,$08e(a6)  ; window $81..$181: both edges on the frame
        move.w  #$2c81,$090(a6)
        move.w  #$2100,$1e4(a6)  ; stop H8 and V8, no fine bits
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        move.w  #$1241,$100(a6)  ; ECSENA, one super-hires bitplane
        move.w  #$0c00,$106(a6)
        move.w  #$0011,$10c(a6)
        move.w  #$0003,$1fc(a6)  ; 64-bit bitplane fetch
        move.w  #$0008,$180(a6)
        move.w  #$0ff0,$182(a6)
        move.l  #$61000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$8380,$096(a6)
.loop:  bra.s   .loop

list:   dc.l    $00e00004,$00e20000
        dc.l    $4001fffe,$01e42100
        dc.l    $5001fffe,$01e42108
        dc.l    $6001fffe,$01e42110
        dc.l    $7001fffe,$01e42118
        dc.l    $8001fffe,$01e42100
        dc.l    $9001fffe,$01e42900
        dc.l    $a001fffe,$01e43100
        dc.l    $b001fffe,$01e43900
        dc.l    $c001fffe,$01e42100
        dc.l    $d001fffe,$01e42908
        dc.l    $e001fffe,$01e43110
        dc.l    $f001fffe,$01e43918
        dc.l    $ffdffffe
        dc.l    $0001fffe,$01e42100
        dc.l    $fffffffe
list_end:
