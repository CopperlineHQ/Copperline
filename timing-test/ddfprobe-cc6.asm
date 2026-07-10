; Raced copper-chunky probe, gen-x-exact per-line copper block.
;
; cc4 (full WAIT + train) and cc5 (h-only masked WAIT + train) both render
; identically on Copperline and vAmiga; the gen-x mosaic still diverges (the
; first raced cell is one lo-res pixel wider on Copperline). The remaining
; difference is the demo's full per-line block: the copper arrives at the
; h-only masked WAIT right around its release point after a 35-MOVE block
; that starts on the PREVIOUS line, so a one-slot difference in cumulative
; copper pacing decides whether the train starts at the WAIT position or a
; cycle later. This probe replicates the decoded gen-x block exactly:
;
;   WAIT (v=L-1, h=$68, maskv=$7F)      ; anchor late on the previous line
;   32x MOVE to COLOR16/COLOR01/... pairs (the demo's palette block)
;   MOVE BPLCON0, $7200
;   MOVE COP2LCL, #dummy                 ; the demo's self-modifying pointer poke
;   WAIT (h=$4E, maskv=$00, maskh=$FE)   ; horizontal-only chase-the-beam WAIT
;   33x MOVE COLOR00                     ; the raced cell train
;
; Display: gen-x registers, plane 1 = $1111 grid, planes 2-6 zero, BPU=7.
; Cells alternate red/green, last write blue (previous-line tail marker).
CUST   equ $dff000
GRID   equ $40000
ZERO   equ $50000
CLIST  equ $60000
FILLW  equ 14336
CELLS  equ 32

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea GRID,a0
        move.w #FILLW-1,d0
.fg:    move.w #$1111,(a0)+      ; grid at native x 3,7,11,...
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
        subq.w #1,d2              ; anchor on the PREVIOUS line
        lsl.w  #8,d2
        add.w  #$69,d2            ; WAIT(v=L-1, h=$68): first word |1
        move.w d2,(a1)+
        move.w #$7ffe,(a1)+       ; maskv=$7F maskh=$FE (demo-exact)
        move.w #15,d3             ; 16 pairs = 32 palette MOVEs (demo-exact)
.floop:
        move.w #$01a0,(a1)+       ; COLOR16 bank poke (not displayed: idx 0/1)
        move.w #$0000,(a1)+
        move.w #$01a4,(a1)+       ; COLOR18 bank poke
        move.w #$0000,(a1)+
        dbra   d3,.floop
        move.w #$0100,(a1)+       ; BPLCON0 $7200 (demo-exact)
        move.w #$7200,(a1)+
        move.w #$0086,(a1)+       ; COP2LCL dummy poke (demo-exact slot cost)
        move.w #$0000,(a1)+
        move.w #$004f,(a1)+       ; WAIT h=$4E, v ignored
        move.w #$00fe,(a1)+       ; maskv=$00 maskh=$FE
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
