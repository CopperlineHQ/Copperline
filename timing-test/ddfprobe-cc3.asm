; Faithful gen-x mosaic display probe (STABLE: copper reloads BPLxPT each frame).
; EXACT gen-x registers DDFSTRT=$48 DDFSTOP=$C0 DIWSTRT=$2881 DIWSTOP=$30C1
; BPLCON0=$7200 (BPU=7). Plane 1 carries a grid line every 4 lo-res px ($8888);
; planes 2-6 zero. Index alternates 0 (cell -> COLOR00 red) / 1 (grid -> COLOR01
; white). Measures the mosaic cell structure with gen-x's actual DDF/DIW.
CUST   equ $dff000
GRID   equ $40000
ZERO   equ $50000
CLIST  equ $60000
FILLW  equ 14336
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea GRID,a0
        move.w #FILLW-1,d0
.fg:    move.w #$8888,(a0)+
        dbra d0,.fg
        lea ZERO,a0
        move.w #FILLW-1,d0
.fz:    clr.w (a0)+
        dbra d0,.fz
        lea CLIST,a1
        move.l #$01007200,(a1)+   ; BPLCON0 BPU=7
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800f00,(a1)+   ; COLOR00 red (cell)
        move.l #$01820fff,(a1)+   ; COLOR01 white (grid)
        move.l #$00920048,(a1)+   ; DDFSTRT $48
        move.l #$009400c0,(a1)+   ; DDFSTOP $C0
        move.l #$008e2881,(a1)+   ; DIWSTRT
        move.l #$009030c1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PTH ($40000 grid)
        move.l #$00e20000,(a1)+
        move.l #$00e40005,(a1)+   ; BPL2PTH ($50000 zero)
        move.l #$00e60000,(a1)+
        move.l #$00e80005,(a1)+   ; BPL3
        move.l #$00ea0000,(a1)+
        move.l #$00ec0005,(a1)+   ; BPL4
        move.l #$00ee0000,(a1)+
        move.l #$00f00005,(a1)+   ; BPL5
        move.l #$00f20000,(a1)+
        move.l #$00f40005,(a1)+   ; BPL6
        move.l #$00f60000,(a1)+
        move.l #$fffffffe,(a1)+   ; end
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
