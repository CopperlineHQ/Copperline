; AGA wide-FMODE scroll fold below the hardwired DDF start (lo-res, 64-bit
; fetch).  Companion to ddfprobe-agafold2, which maps the fold boundary as
; a function of the fetch-unit phase for DDFSTRT values inside the hardware
; window.  This probe covers DDFSTRT values BELOW the hardwired start $18.
;
; On ECS Agnus and AGA Alice a DDFSTRT comparator match before $18 only
; latches BPHSTART; the fetch run starts when SHW rises at $18, on every
; line (SHW is cleared at each line end).  The run therefore sits on the
; $18 unit phase (24 cck past the 32-cck grid), whose fold boundary
; (2*24 + 16 = 64 px) lies past the whole 0..63 tap range: every tap
; renders linearly, exactly as DDFSTRT $18 does.  Taking the fold phase
; from the raw register instead ($10 -> phase 16 -> boundary 48) folds
; taps 48..63 one full gulp left -- the Skeleton Krew CD32 regression
; class (issue #836: lo-res 7 planes, FMODE=$000F, DDFSTRT $10 / DDFSTOP
; $A0, 48-byte rows, BPLCON1 taps swept across 0..63 while the camera
; scrolls; the folded frames showed the playfield 64 px left with black
; past the fetched data on the right).
;
; 16 bands of 8 lines; every line re-writes DDFSTRT, BPLCON1 and BPL1PT,
; so the mid-row marker x per band IS the placement map.  DDFSTOP $A0 (the
; Skeleton Krew value) is shared by every band.  Starts $10..$18 all run
; from $18 and fetch 6 units (24 words); the in-window $50 control bands
; run from $50 (4 units) and place their picture one 64-cck grid step
; (256 raw px) right of the sub-$18 bands.
;
; Expected relative map from band 0 (raw px; marker x = x0 + tap * 2,
; a fold subtracts one gulp = 128 raw px):
;   bands 0-3   ($18, taps 0/47/48/63):        0, +94, +96, +126 (linear)
;   bands 4-8   ($10, taps 0/47/48/56/63):     0, +94, +96, +112, +126
;   bands 9-12  ($12/$14/$16, taps 52/56/60/63): +104, +112, +120, +126
;   bands 13-15 ($50, taps 47/48/63):          +350, +224, +254
; i.e. bands 0-12 render linearly while the in-window phase-16 control
; folds from tap 48 (agafold2 band 12, FS-UAE-verified).
;
; vAmiga-verified (5.0b1 A1200_2MB AGA setup, 2026-10-27): every band
; position agrees (0.051%, 104 px: one raster line at the $16 -> $50 band
; edge, the same single-line transition disagreement as agafold2).  The
; register-phase model folded bands 6-12 one gulp (128 raw px) left.
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
        move.w #3-1,d0            ; w2..w4 empty
.z:     clr.w (a0)+
        dbra d0,.z
        move.w #$ff0f,(a0)+       ; w5: mid marker (8px bar + 4px tick)
        move.w #4-1,d0            ; w6..w9 empty
.z1:    clr.w (a0)+
        dbra d0,.z1
        move.w #$ffff,(a0)+       ; w10: row-end bar
        move.w #$ffff,(a0)+       ; w11
        move.w #8192-1,d0
.z2:    clr.w (a0)+
        dbra d0,.z2
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01040024,(a1)+   ; BPLCON2
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$010c0011,(a1)+   ; BPLCON4
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920018,(a1)+   ; DDFSTRT (per-band rewrite below)
        move.l #$009400a0,(a1)+   ; DDFSTOP $A0 (Skeleton Krew value)
        move.l #$008e287b,(a1)+   ; DIWSTRT $7B (into left overscan)
        move.l #$00902dc5,(a1)+   ; DIWSTOP $1C5
        move.l #$01fc0003,(a1)+   ; FMODE = BPL64
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea bands(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d6           ; DDFSTRT for this band
        move.w (a2)+,d7           ; BPLCON1 for this band
        moveq #8-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07): DDFSTRT lands before $10
        move.w #$fffe,(a1)+
        move.w #$0092,(a1)+       ; DDFSTRT
        move.w d6,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w d7,(a1)+
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
; Per band: DDFSTRT, BPLCON1.
; AGA lores raw scroll L both playfields: low byte (L&15)|(L&15)<<4,
; high byte ((L>>4)&3)<<2 | ((L>>4)&3)<<6 (H4-H5 pf1 bits 10-11, pf2
; bits 14-15).
bands:
        dc.w $0018,$0000          ; band  0: $18 (hard start), tap  0 (reference)
        dc.w $0018,$88FF          ; band  1: $18, tap 47
        dc.w $0018,$CC00          ; band  2: $18, tap 48
        dc.w $0018,$CCFF          ; band  3: $18, tap 63
        dc.w $0010,$0000          ; band  4: $10 (below hard start), tap  0
        dc.w $0010,$88FF          ; band  5: $10, tap 47
        dc.w $0010,$CC00          ; band  6: $10, tap 48 (raw phase-16 boundary)
        dc.w $0010,$CC88          ; band  7: $10, tap 56
        dc.w $0010,$CCFF          ; band  8: $10, tap 63
        dc.w $0012,$CC44          ; band  9: $12, tap 52 (raw phase-18 boundary)
        dc.w $0014,$CC88          ; band 10: $14, tap 56 (raw phase-20 boundary)
        dc.w $0016,$CCCC          ; band 11: $16, tap 60 (raw phase-22 boundary)
        dc.w $0016,$CCFF          ; band 12: $16, tap 63
        dc.w $0050,$88FF          ; band 13: $50 (in window, phase 16), tap 47
        dc.w $0050,$CC00          ; band 14: $50, tap 48 (folds)
        dc.w $0050,$CCFF          ; band 15: $50, tap 63 (folds)
