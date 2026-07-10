; Raced copper-chunky COLOR00 vs bitplane-fetch phase probe (gen-x mosaic).
;
; ddfprobe-cc3 proved the STATIC display path (gen-x registers, grid bitmap,
; fixed palette) matches FS-UAE, and ddfprobe-cc raced COLOR00 over a ZEROED
; bitmap (no bitmap anchor). Neither measures where a raced COLOR00 write
; train lands RELATIVE to the fetched bitplane content -- which is exactly
; where the gen-x mosaic's "wide left column" lives: the first raced write's
; colour must not appear before the row's first fetched pixel.
;
; This probe combines both: gen-x's exact registers (DDFSTRT=$48 DDFSTOP=$C0
; DIWSTRT=$2881 DIWSTOP=$30C1 BPLCON0=$7200 BPU=7), plane 1 = $1111 (grid
; line every 4 lo-res px at native x 3,7,11... like the demo's mosaic grid),
; planes 2-6 zero, COLOR01 black. Every display line the copper waits for
; line start, burns filler MOVEs to a scratch colour register, then writes
; COLOR00 back to back every 4 cck from hpos ~$50 (33 writes, like gen-x),
; alternating red/green so each landing edge is visible.
;
; Measure the FIRST cell (before the first grid line) against the body cells
; on Copperline vs vAmiga vs FS-UAE/real: if the first raced colour bleeds
; left of native pixel 0, the copper colour-write landing sits early relative
; to the bitplane pipeline.
CUST   equ $dff000
GRID   equ $40000
ZERO   equ $50000
CLIST  equ $60000
FILLW  equ 14336
CELLS  equ 33                    ; COLOR00 writes per line, like gen-x
FILLM  equ 19                    ; filler MOVEs before the train (tunes h=$50)

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
        move.l #$01007200,(a1)+   ; BPLCON0 BPU=7 (6 planes on OCS)
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800000,(a1)+   ; COLOR00 black outside the raced band
        move.l #$01820000,(a1)+   ; COLOR01 black (grid)
        move.l #$00920048,(a1)+   ; DDFSTRT $48 (gen-x)
        move.l #$009400c0,(a1)+   ; DDFSTOP $C0
        move.l #$008e2881,(a1)+   ; DIWSTRT (gen-x)
        move.l #$009030c1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PTH ($40000 grid)
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

        move.w #$50,d1            ; first raced line (inside DIW top=$28)
.vloop:
        move.w d1,d2
        lsl.w  #8,d2
        addq.w #1,d2              ; WAIT(V,$00) = (V<<8)|$01, mask $FF00
        move.w d2,(a1)+
        move.w #$ff00,(a1)+
        move.w #FILLM-1,d3        ; filler MOVEs so the train starts at ~h=$50
.floop:
        move.w #$01a2,(a1)+       ; COLOR17: scratch, not displayed (index 0/1)
        move.w #$0000,(a1)+
        dbra   d3,.floop
        move.w #CELLS-1,d3
.cloop:
        move.w #$0180,(a1)+       ; COLOR00
        btst   #0,d3
        beq.s  .green
        move.w #$0f00,(a1)+       ; red
        bra.s  .cnext
.green:
        move.w #$00f0,(a1)+       ; green
.cnext:
        dbra   d3,.cloop
        move.w #$0180,(a1)+       ; end of band: back to black
        move.w #$0000,(a1)+
        addq.w #1,d1
        cmp.w  #$c0,d1
        bne.s  .vloop
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
