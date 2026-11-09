; AGA wide-fetch scroll scaling observation probe.
; 48 bands, four lines each, at beam lines $3C..$FB.
; Six groups of eight bands: hires BPL32 DDF $40; hires BPL64 $40/$44;
; SHRES BPL32 $40; SHRES BPL64 $40/$42. Each group sweeps legacy
; lores scroll taps 0,7,8,15,16,23,24,31 (both playfields).
; The $40 groups bracket both a fixed-native-sample pipeline and the
; current 8-CCK pipeline; the off-grid groups test the phase term.
; Every line resets BPL1PT to an asymmetric, nonperiodic marker row.
; Compare band interiors; first lines also expose mode-change latency.
; This is an observation fixture, not a hardware-derived expected result.
; No physical AGA capture was available when this probe was added.
CUST    equ $dff000
BMP     equ $40000
CLIST   equ $60000
NBAND   equ 48
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #8192-1,d0
.zero:  clr.w (a0)+
        dbra d0,.zero
        move.w #$f000,BMP+16
        move.w #$ff0f,BMP+32
        move.w #$ffff,BMP+50
        lea CLIST,a1
        move.l #$01040024,(a1)+
        move.l #$01060c00,(a1)+
        move.l #$010c0011,(a1)+
        move.l #$01080000,(a1)+
        move.l #$010a0000,(a1)+
        move.l #$01800008,(a1)+
        move.l #$01820fff,(a1)+
        move.l #$009400a0,(a1)+
        move.l #$008e2c81,(a1)+
        move.l #$00902cc1,(a1)+
        lea bands(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3
.band:  move.w (a2)+,d4
        move.w (a2)+,d6
        move.w (a2)+,d7
        move.w (a2)+,d1
        moveq #4-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0100,(a1)+
        move.w d4,(a1)+
        move.w #$01fc,(a1)+
        move.w d6,(a1)+
        move.w #$0092,(a1)+
        move.w d7,(a1)+
        move.w #$0102,(a1)+
        move.w d1,(a1)+
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+
        add.w #$0100,d3
        dbra d5,.line
        dbra d2,.band
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)
.loop:  bra.s .loop
        cnop 0,2
bands:
        dc.w $9201,$0001,$0040,$0000 ; band  0, tap 0
        dc.w $9201,$0001,$0040,$0077 ; band  1, tap 7
        dc.w $9201,$0001,$0040,$0088 ; band  2, tap 8
        dc.w $9201,$0001,$0040,$00ff ; band  3, tap 15
        dc.w $9201,$0001,$0040,$4400 ; band  4, tap 16
        dc.w $9201,$0001,$0040,$4477 ; band  5, tap 23
        dc.w $9201,$0001,$0040,$4488 ; band  6, tap 24
        dc.w $9201,$0001,$0040,$44ff ; band  7, tap 31
        dc.w $9201,$0003,$0040,$0000 ; band  8, tap 0
        dc.w $9201,$0003,$0040,$0077 ; band  9, tap 7
        dc.w $9201,$0003,$0040,$0088 ; band 10, tap 8
        dc.w $9201,$0003,$0040,$00ff ; band 11, tap 15
        dc.w $9201,$0003,$0040,$4400 ; band 12, tap 16
        dc.w $9201,$0003,$0040,$4477 ; band 13, tap 23
        dc.w $9201,$0003,$0040,$4488 ; band 14, tap 24
        dc.w $9201,$0003,$0040,$44ff ; band 15, tap 31
        dc.w $9201,$0003,$0044,$0000 ; band 16, tap 0
        dc.w $9201,$0003,$0044,$0077 ; band 17, tap 7
        dc.w $9201,$0003,$0044,$0088 ; band 18, tap 8
        dc.w $9201,$0003,$0044,$00ff ; band 19, tap 15
        dc.w $9201,$0003,$0044,$4400 ; band 20, tap 16
        dc.w $9201,$0003,$0044,$4477 ; band 21, tap 23
        dc.w $9201,$0003,$0044,$4488 ; band 22, tap 24
        dc.w $9201,$0003,$0044,$44ff ; band 23, tap 31
        dc.w $1241,$0001,$0040,$0000 ; band 24, tap 0
        dc.w $1241,$0001,$0040,$0077 ; band 25, tap 7
        dc.w $1241,$0001,$0040,$0088 ; band 26, tap 8
        dc.w $1241,$0001,$0040,$00ff ; band 27, tap 15
        dc.w $1241,$0001,$0040,$4400 ; band 28, tap 16
        dc.w $1241,$0001,$0040,$4477 ; band 29, tap 23
        dc.w $1241,$0001,$0040,$4488 ; band 30, tap 24
        dc.w $1241,$0001,$0040,$44ff ; band 31, tap 31
        dc.w $1241,$0003,$0040,$0000 ; band 32, tap 0
        dc.w $1241,$0003,$0040,$0077 ; band 33, tap 7
        dc.w $1241,$0003,$0040,$0088 ; band 34, tap 8
        dc.w $1241,$0003,$0040,$00ff ; band 35, tap 15
        dc.w $1241,$0003,$0040,$4400 ; band 36, tap 16
        dc.w $1241,$0003,$0040,$4477 ; band 37, tap 23
        dc.w $1241,$0003,$0040,$4488 ; band 38, tap 24
        dc.w $1241,$0003,$0040,$44ff ; band 39, tap 31
        dc.w $1241,$0003,$0042,$0000 ; band 40, tap 0
        dc.w $1241,$0003,$0042,$0077 ; band 41, tap 7
        dc.w $1241,$0003,$0042,$0088 ; band 42, tap 8
        dc.w $1241,$0003,$0042,$00ff ; band 43, tap 15
        dc.w $1241,$0003,$0042,$4400 ; band 44, tap 16
        dc.w $1241,$0003,$0042,$4477 ; band 45, tap 23
        dc.w $1241,$0003,$0042,$4488 ; band 46, tap 24
        dc.w $1241,$0003,$0042,$44ff ; band 47, tap 31
