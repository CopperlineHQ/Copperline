; Copper-chunky COLOR00 write-landing probe.
;
; The gen-x rotozoom mosaic is drawn "copper chunky": with BPU=7 over a zeroed
; bitplane (every pixel index 0), the copper writes COLOR00 back to back across
; each line, so each write paints one ~8px cell. The mosaic's "wide left column"
; is therefore a question of where the FIRST copper COLOR00 write lands relative
; to the display start -- a copper colour-write-landing timing issue, not bitmap
; data. This probe reproduces that: a zeroed 6-plane BPU=7 display, and a copper
; list (built at runtime at $60000) that, every line, waits for line start then
; writes COLOR00 back to back alternating white/blue -- an 8px white/blue stripe
; ruler. Compare the first stripe's width and the stripe boundaries on Copperline
; vs FS-UAE: a difference is a copper COLOR00 landing bug.
CUST   equ $dff000
ZERO   equ $50000
CLIST  equ $60000
FILLW  equ 14336
CELLS  equ 44                    ; COLOR00 writes per line

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea ZERO,a0
        move.w #FILLW-1,d0
.fz:    clr.w (a0)+
        dbra d0,.fz

        ; ---- build copper list at $60000 ----
        lea CLIST,a1
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$008e2c50,(a1)+   ; DIWSTRT wide (H=$50)
        move.l #$00902cd0,(a1)+   ; DIWSTOP wide (H=$D0)
        move.l #$00920038,(a1)+   ; DDFSTRT $38
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0
        move.l #$01007200,(a1)+   ; BPLCON0 BPU=7
        move.l #$00e00005,(a1)+   ; BPL1PTH ($50000)
        move.l #$00e20000,(a1)+   ; BPL1PTL
        move.l #$00e40005,(a1)+   ; BPL2PTH
        move.l #$00e60000,(a1)+
        move.l #$00e80005,(a1)+   ; BPL3PTH
        move.l #$00ea0000,(a1)+
        move.l #$00ec0005,(a1)+   ; BPL4PTH
        move.l #$00ee0000,(a1)+
        move.l #$00f00005,(a1)+   ; BPL5PTH
        move.l #$00f20000,(a1)+
        move.l #$00f40005,(a1)+   ; BPL6PTH
        move.l #$00f60000,(a1)+

        move.w #$40,d1            ; first line
.vloop:
        move.w d1,d2
        lsl.w  #8,d2
        addq.w #1,d2              ; WAIT(V,$00) = (V<<8)|1
        move.w d2,(a1)+
        move.w #$ff00,(a1)+
        move.w #CELLS-1,d3
.cloop:
        move.w #$0180,(a1)+       ; COLOR00 register
        btst   #0,d3
        beq.s  .white
        move.w #$000f,(a1)+       ; blue
        bra.s  .cnext
.white:
        move.w #$0fff,(a1)+       ; white
.cnext:
        dbra   d3,.cloop
        addq.w #1,d1
        cmp.w  #$c0,d1
        bne.s  .vloop
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
