; AGA wide-FMODE scroll-fold placement probe (lo-res, 32-bit fetch).
; Agnus masks an off-grid DDFSTRT DOWN to the fetch-unit grid, so the data
; arrives early relative to the programmed start; Denise's reload
; comparator window is anchored at that early fetch start, so BPLCON1
; scroll taps folding into the last `earliness` px of the gulp window
; already see the NEXT gulp's data and the playfield sits one full gulp
; left: display delay = ((tap + earliness) mod gulp) - earliness.
;
; The constellation clones Alien Breed II AGA's playfield (the issue #248
; horizontal scroll regression example): lo-res, FMODE=$0005 (BPL32|SPR32,
; 32-px gulps), DDFSTRT $24 (4 cck past the 16-cck unit grid -> masked to
; $20, earliness 8 px), DDFSTOP $D4, DIW $7B..$1C5 (into the overscan both
; sides). Its scroller pairs the folded taps (raw AGA scroll 57..63, tap
; 25..31) with a one-gulp bitplane-pointer step; without the fold the pan
; jumps 32 px for 4 of every 16 frames.
;
; 16 bands of 8 lines sweep (BPL1PT byte offset, extended BPLCON1) pairs;
; every line re-anchors the same marker bitmap, so the mid-row marker x per
; band IS the placement map: x = x0 + tap*2 - off*16 - 64*[tap >= 24],
; with tap = raw scroll & 31 (32-bit fetch masks the range to one gulp) and
; the fold boundary at gulp - earliness = 24. The pointer's byte offset
; must never change the fold (bands 8-15: same map, shifted by the offset).
;
; FS-UAE-verified (WinUAE core, A1200/KS3.1, 2026-07-22): all 16 band
; positions match Copperline's render exactly (relative map 0,+2,+4,+16,
; -44,-40,-32,-28,-108,-104,-96,-92,-64,-60,-172,-156 raw px from band 0).
; Re-verified 2026-08-27 on vAmiga 5.0b1's new A1200_2MB AGA setup: the
; render matches exactly (0 of 202628 pixels differ under the colour
; correspondence, vAmiga's raw frame starting two lines later).  The fold
; boundary and the lo-res BPL32 scaling are pinned; hi-res/SHRES scaling is
; not.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000
NBAND  equ 16
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #$ffff,(a0)+       ; w0: row-start bar
        move.w #$f000,(a0)+       ; w1: tick
        move.w #8-1,d0            ; w2..w9 empty
.z:     clr.w (a0)+
        dbra d0,.z
        move.w #$ff0f,(a0)+       ; w10: mid marker (8px bar + 4px tick)
        move.w #10-1,d0           ; w11..w20 empty
.z1:    clr.w (a0)+
        dbra d0,.z1
        move.w #0,(a0)+           ; w21 empty
        move.w #$ffff,(a0)+       ; w22: row-end bar
        move.w #$ffff,(a0)+       ; w23
        move.w #8192-1,d0
.z2:    clr.w (a0)+
        dbra d0,.z2
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01040024,(a1)+   ; BPLCON2 (game uses $224; sprites moot)
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$010c0011,(a1)+   ; BPLCON4
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920024,(a1)+   ; DDFSTRT $24 (game value, off-grid)
        move.l #$009400d4,(a1)+   ; DDFSTOP $D4
        move.l #$008e287b,(a1)+   ; DIWSTRT $7B (into left overscan)
        move.l #$00902dc5,(a1)+   ; DIWSTOP $1C5
        move.l #$01fc0005,(a1)+   ; FMODE = BPL32|SPR32
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea bands(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d6           ; BPL1PTL for this band (BMP + offset)
        move.w (a2)+,d7           ; BPLCON1 for this band
        moveq #8-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07)
        move.w #$fffe,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w d7,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w d6,(a1)+
        add.w #$0100,d3
        dbra d5,.line
        dbra d2,.band
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
        cnop 0,2
; Per band: BPL1PTL offset, BPLCON1.
; AGA lores raw scroll L both playfields: low byte (L&15)|(L&15)<<4,
; H6-H7 = (L>>4) in bits 10-11 (pf1) and 14-15 (pf2).
; Expected mask-32 model: marker x = x0 + (L & 31) * 2 - off_px * 2.
bands:
        dc.w $0000,$00FF          ; band  0: off 0, raw 15   -> ref x0+30
        dc.w $0000,$4400          ; band  1: off 0, raw 16   -> x0+32
        dc.w $0000,$CC11          ; band  2: off 0, raw 49   -> x0+34
        dc.w $0000,$CC77          ; band  3: off 0, raw 55   -> x0+46
        dc.w $0000,$CC99          ; band  4: off 0, raw 57   -> x0+50 (game: -32?)
        dc.w $0000,$CCBB          ; band  5: off 0, raw 59   -> x0+54
        dc.w $0000,$CCFF          ; band  6: off 0, raw 63   -> x0+62
        dc.w $0000,$0011          ; band  7: off 0, raw 1    -> x0+2
        dc.w $0004,$CC99          ; band  8: off 4, raw 57 (ptr mod8=4)
        dc.w $0004,$CCBB          ; band  9: off 4, raw 59
        dc.w $0004,$CCFF          ; band 10: off 4, raw 63
        dc.w $0004,$0011          ; band 11: off 4, raw 1
        dc.w $0004,$00FF          ; band 12: off 4, raw 15
        dc.w $0004,$CC11          ; band 13: off 4, raw 49
        dc.w $0008,$CC99          ; band 14: off 8, raw 57 (ptr mod8=0)
        dc.w $0008,$0011          ; band 15: off 8, raw 1
