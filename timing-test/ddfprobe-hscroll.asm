; BPLCON1 hi-res scroll placement probe (hi-res, FMODE=0).
; 9 bands of 8 lines walk BPLCON1 through $00,$11,$22..$88 on the Kickstart
; 2.05 insert-disk display constellation: hi-res 1 plane, DDFSTRT $40,
; DDFSTOP $D0 (38 words = 608 px), DIWSTRT hstart $95, DIWSTOP hstop $1AD
; (560 px window), so the fetched row overhangs the window on both sides.
; The copper re-anchors BPL1PT to the same marker bitmap on every line, so
; each band displays identical data and the marker positions per band ARE
; the scroll->placement map. The OCS/ECS scroll nibble counts lo-res pixels
; (Denise compares the low 3 nibble bits against its pixel counter in
; hi-res), so one scroll step moves the picture 2 hi-res px and band $88
; must render exactly like band $00.
; Markers: w0=$FFFF w1=$F000 (bar + tick at the row start: the visible
; sliver at the window's LEFT edge appears from band $33 and grows 2 px per
; band) and w36=w37=$FFFF (row-end bar: its head starts 8 px inside the
; window's RIGHT edge at $00, recedes 2 px per band, and is exactly clipped
; at the DIW stop from band $44 up). Band $88 must repeat band $00.
; vAmiga-verified band by band (A500 ECS). Regression example: Kickstart
; 2.05's boot screen (BPLCON1 $44) halved to a 1-px-per-step map clips the
; first text column at the left edge and leaks the negative-modulo overlap
; words (the next row's first characters) into the right edge.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000
NBAND  equ 9
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #$ffff,(a0)+       ; w0: row-start bar
        move.w #$f000,(a0)+       ; w1: window-edge tick
        move.w #34-1,d0           ; w2..w35 empty
.z:     clr.w (a0)+
        dbra d0,.z
        move.w #$ffff,(a0)+       ; w36: row-end bar (head at the DIW stop)
        move.w #$ffff,(a0)+       ; w37
        move.w #8192-1,d0         ; keep the un-anchored scan area blank
.z2:    clr.w (a0)+
        dbra d0,.z2
        lea CLIST,a1
        move.l #$01009200,(a1)+   ; BPLCON0: 1 plane, hi-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920040,(a1)+   ; DDFSTRT $40 (KS 2.05 boot screen)
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0
        move.l #$008e2c95,(a1)+   ; DIWSTRT: hstart $95
        move.l #$0090f4ad,(a1)+   ; DIWSTOP: hstop $1AD
        move.l #$00e00004,(a1)+   ; BPL1PTH = $40000
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea scrolls(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d4           ; BPLCON1 for this band
        moveq #8-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07)
        move.w #$fffe,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w d4,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w #$0000,(a1)+
        add.w #$0100,d3
        dbra d5,.line
        dbra d2,.band
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
        cnop 0,2
scrolls: dc.w $0000,$0011,$0022,$0033,$0044,$0055,$0066,$0077,$0088
