; Raced copper-chunky probe, gen-x-faithful line launch: the COLOR00 train is
; released by a HORIZONTAL-ONLY masked WAIT (maskv=$00, h=$4E) exactly like
; the gen-x mosaic's per-line copper block (see the decoded list in the
; repo notes: WAIT v=xx h=4e maskv=00 maskh=fe, then 33 MOVEs to COLOR00).
;
; ddfprobe-cc4 used a normal full WAIT and rendered IDENTICALLY on Copperline
; and vAmiga; the demo diverges (Copperline paints the first raced colour one
; lo-res pixel earlier relative to the fetched bitmap). If cc5 reproduces the
; divergence, the bug is the copper's resume/landing phase after an h-only
; masked WAIT, not the colour-write landing itself.
;
; Same display as cc4: gen-x registers, plane 1 = $1111 grid (native x 3,7,..),
; planes 2-6 zero, BPU=7. Per line: palette-block-like filler MOVEs, then
; WAIT (any-v, h=$4E), then 33 COLOR00 MOVEs alternating red/green. No
; trailing background reset (the demo leaves the last cell colour live), so
; the pre-fetch pixels show the PREVIOUS line's last colour on hardware.
; The last write is BLUE so "previous-line tail" (blue) is distinguishable
; from "first write early" (red/green) at the row's left edge.
CUST   equ $dff000
GRID   equ $40000
ZERO   equ $50000
CLIST  equ $60000
FILLW  equ 14336
CELLS  equ 32                    ; alternating writes before the final blue
FILLM  equ 16                    ; palette-block-like filler MOVEs

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea GRID,a0
        move.w #FILLW-1,d0
.fg:    move.w #$1111,(a0)+      ; grid at native x 3,7,11,... (demo phase)
        dbra d0,.fg
        lea ZERO,a0
        move.w #FILLW-1,d0
.fz:    clr.w (a0)+
        dbra d0,.fz

        ; ---- build copper list at $60000 ----
        lea CLIST,a1
        move.l #$01007200,(a1)+   ; BPLCON0 BPU=7
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800000,(a1)+   ; COLOR00 black before the band
        move.l #$01820000,(a1)+   ; COLOR01 black (grid)
        move.l #$00920048,(a1)+   ; DDFSTRT $48 (gen-x)
        move.l #$009400c0,(a1)+   ; DDFSTOP $C0
        move.l #$008e2881,(a1)+   ; DIWSTRT (gen-x)
        move.l #$009030c1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PT ($40000 grid)
        move.l #$00e20000,(a1)+
        move.l #$00e40005,(a1)+   ; BPL2PT ($50000 zero)
        move.l #$00e60000,(a1)+
        move.l #$00e80005,(a1)+   ; BPL3
        move.l #$00ea0000,(a1)+
        move.l #$00ec0005,(a1)+   ; BPL4
        move.l #$00ee0000,(a1)+
        move.l #$00f00005,(a1)+   ; BPL5
        move.l #$00f20000,(a1)+
        move.l #$00f40005,(a1)+   ; BPL6
        move.l #$00f60000,(a1)+

        move.w #$50,d1            ; first raced line
.vloop:
        move.w d1,d2
        lsl.w  #8,d2
        addq.w #1,d2              ; full WAIT(V,$00): line anchor
        move.w d2,(a1)+
        move.w #$ff00,(a1)+
        move.w #FILLM-1,d3        ; palette-block-like fillers (demo: 32 pokes)
.floop:
        move.w #$01a2,(a1)+       ; scratch colour register (not displayed)
        move.w #$0000,(a1)+
        dbra   d3,.floop
        move.w #$004f,(a1)+       ; WAIT h=$4E, v ignored: first word (0<<8)|$4E|1
        move.w #$00fe,(a1)+       ;   mask word BFD=0 maskv=$00 maskh=$FE (demo-exact)
        move.w #CELLS-1,d3
.cloop:
        move.w #$0180,(a1)+       ; COLOR00 train
        btst   #0,d3
        beq.s  .green
        move.w #$0f00,(a1)+       ; red
        bra.s  .cnext
.green:
        move.w #$00f0,(a1)+       ; green
.cnext:
        dbra   d3,.cloop
        move.w #$0180,(a1)+       ; final write: BLUE tail marker
        move.w #$000f,(a1)+
        addq.w #1,d1
        cmp.w  #$c0,d1
        bne.s  .vloop
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
