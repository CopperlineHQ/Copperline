; AGA wide-FMODE scroll-fold phase-sweep probe (lo-res, 64-bit fetch).
; Companion to ddfprobe-agafold (the 32-bit AB2 constellation): maps the
; fold boundary as a FUNCTION OF THE DDFSTRT PHASE within the fetch unit,
; on the constellation SANITY Roots II AGA uses for its swirl/kaleidoscope
; screens (issue #371): lo-res, FMODE=$0003 (BPL64, 64-px gulps, 32-cck
; unit), DDFSTRT $58/$38 -> phase 24 cck past the unit grid, large AGA
; BPLCON1 taps (16, 17, 30, 43 live in the demo).
;
; The question each band answers: for fetch-unit phase P (DDFSTRT mod 32
; cck) and scroll tap T, does the playfield render at the linear position
; (delay T) or one full gulp left (delay T - 64)? Denise's reload
; comparator runs on the ABSOLUTE hpos gulp grid (WinUAE cycle-exact
; delay_cycles model), so the fold boundary is the data-arrival distance
; past the grid point, 2*P + 16 px, saturating (NOT wrapping) at the top
; of the tap range -- not the last-earliness window (64 - 2*P) the first
; agafold model used: phase 24 or 28 -> boundary past 63 -> no folds at
; all (the demo's taps render linearly, matching real hardware), an
; on-grid start folds from the 16-px pipeline alone, while the
; FS-UAE-verified AB2 map (32-px gulps, phase 4 -> boundary 24) is
; reproduced by both models and cannot separate them.
;
; 16 bands of 8 lines; every line re-writes DDFSTRT, BPLCON1 and BPL1PT,
; so the mid-row marker x per band IS the placement map. DDFSTOP $90 and
; the $40 unit anchor are shared by every band (starts $40..$5C all mask
; down to $40): 3 fetch units, 12 words per row.
;
; FS-UAE-verified (WinUAE core, A1200/KS3.1, 2026-08-03): all 16 band
; positions match Copperline's render exactly. Relative map from band 0
; in raw px: +30,+32,+34,+94,+96,+126 (bands 1-6, phase 24: linear),
; +78,+110,+112 (bands 7-9, phase 28: linear -- the boundary saturates),
; +16,-16 (bands 10/13, on-grid: tap 8 linear, tap 56 folded),
; -16,-32 (bands 11/12), -10,-18 (bands 14/15). vAmiga is OCS/ECS-only
; and cannot arbitrate AGA; hi-res/SHRES pipeline scaling is not pinned.
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
        move.l #$00920058,(a1)+   ; DDFSTRT (per-band rewrite below)
        move.l #$00940090,(a1)+   ; DDFSTOP $90 (Roots II swirl value)
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
        move.w d0,(a1)+           ; WAIT (v,$07)
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
; bits 14-15). Linear placement model: marker x = x0 + (L & 63) * 2.
; A fold subtracts one gulp (128 raw px).
bands:
        dc.w $0058,$0000          ; band  0: phase 24, tap  0 (reference)
        dc.w $0058,$00FF          ; band  1: phase 24, tap 15
        dc.w $0058,$4400          ; band  2: phase 24, tap 16 (Roots pf2)
        dc.w $0058,$4411          ; band  3: phase 24, tap 17 (Roots pf1)
        dc.w $0058,$88FF          ; band  4: phase 24, tap 47
        dc.w $0058,$CC00          ; band  5: phase 24, tap 48
        dc.w $0058,$CCFF          ; band  6: phase 24, tap 63
        dc.w $005C,$8877          ; band  7: phase 28, tap 39
        dc.w $005C,$CC77          ; band  8: phase 28, tap 55
        dc.w $005C,$CC88          ; band  9: phase 28, tap 56
        dc.w $0040,$0088          ; band 10: on-grid,  tap  8
        dc.w $0048,$CC88          ; band 11: phase  8, tap 56
        dc.w $0050,$CC00          ; band 12: phase 16, tap 48
        dc.w $0040,$CC88          ; band 13: on-grid,  tap 56
        dc.w $0044,$CCBB          ; band 14: phase  4, tap 59
        dc.w $0044,$CC77          ; band 15: phase  4, tap 55
