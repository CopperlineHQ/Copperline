; Manual (DATA-armed) sprite bar position probe.
;
; The gen-x mosaic masks its raced copper-chunky left column with a manual
; sprite-7 bar: POS/CTL/DATA/DATB written once by the copper (sprite DMA
; off), then no sprite register writes for the rest of the scene. Denise
; has no vertical comparator, so the armed latch serializes at the POS
; position on every line of every frame. This probe isolates the two
; render paths for that bar so their x positions can be compared against
; vAmiga / FS-UAE / real hardware, with a 16-px ruler bitmap as the
; horizontal reference:
;
;   SPR7 (red, index 3):   POS=$284F CTL=$3003 DATA/DATB=$F000, written at
;                          v=$F0 each frame. Lines $2C..$F0 render from the
;                          latch carried across the frame boundary; lines
;                          $F0..$12C from the same-frame writes. A kink at
;                          v=$F0 means the two paths place differently.
;   SPR6 (green, index 1): POS=$2857 CTL=$3002 DATA=$F000 DATB=0, armed at
;                          v=$50 each frame after a CTL disarm at v=$F0:
;                          the bar starts cleanly at v=$50 (event path).
;
; Sprite DMA stays off (DMACON=$8380) and SPRxPT are never written, like
; the demo scene.
CUST   equ $dff000
RULER  equ $40000
CLIST  equ $60000
FILLW  equ 14336

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea RULER,a0
        move.w #FILLW-1,d0
.fr:    move.w #$8000,(a0)+      ; 1 white px every 16 lo-res px
        dbra d0,.fr

        ; ---- build copper list at $60000 ----
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, colour on
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT $38
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01800113,(a1)+   ; COLOR00 dark blue background
        move.l #$01820fff,(a1)+   ; COLOR01 white ruler
        move.l #$01ba00f0,(a1)+   ; COLOR29 green (SPR6 index 1)
        move.l #$01be0f00,(a1)+   ; COLOR31 red (SPR7 index 3)
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+

        move.l #$5001ff00,(a1)+   ; WAIT v=$50
        move.l #$016cf000,(a1)+   ; SPR6DATA: arm the green bar (event path)
        move.l #$016e0000,(a1)+   ; SPR6DATB

        move.l #$f001ff00,(a1)+   ; WAIT v=$F0
        move.l #$0170284f,(a1)+   ; SPR7POS (the demo bar's exact words)
        move.l #$01723003,(a1)+   ; SPR7CTL (disarms)
        move.l #$0174f000,(a1)+   ; SPR7DATA (re-arms: latched above, fresh below)
        move.l #$0176f000,(a1)+   ; SPR7DATB
        move.l #$01682857,(a1)+   ; SPR6POS (16 lo-res px right of SPR7)
        move.l #$016a3002,(a1)+   ; SPR6CTL (disarms until the v=$50 DATA)
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN (sprite DMA off)
.l:     bra.s .l
