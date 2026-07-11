; Copper COLOR00 landing vs the DIW border transition under the Shadow of
; the Beast title screen's exact display load.
;
; ddfprobe-sotb measured the landing with a single lores bitplane and found
; Copperline == vAmiga (write visible ~2 cck after the WAIT position). The
; game runs a much heavier configuration: BPLCON0=$6600 (6 planes, lores,
; dual playfield), DDFSTRT=$38 / DDFSTOP=$D0 (20-word overscan fetch),
; DIWSTRT.H=$90 / DIWSTOP.H=$1B0, and toggles COLOR00 with WAIT (v,$40) /
; WAIT (v,$D0) -- wait positions that only tuck the toggles against the
; window edges if the 6-plane fetch starves the copper's MOVE by several
; more colour clocks. This probe replicates that configuration exactly:
; plane 1 alternates solid (PF1 colour 1 window reference) and zero rows,
; planes 2-6 stay zero, and the SotB band lines toggle COLOR00 at the
; game's wait positions. Compare the blue band's edges against the white
; reference on Copperline vs vAmiga (vs real hardware).
CUST   equ $dff000
SOLID  equ $40000
ZERO   equ $40100
CLIST  equ $60000
ROWB   equ 40                    ; 20 fetched words = 40 bytes per row

        lea CUST,a6
        move.w #$7fff,$9a(a6)    ; INTENA clear
        move.w #$7fff,$9c(a6)    ; INTREQ clear
        move.w #$7fff,$96(a6)    ; DMACON clear

        lea SOLID,a0
        moveq #ROWB/4-1,d0
.fs:    move.l #$ffffffff,(a0)+
        dbra d0,.fs
        lea ZERO,a0
        moveq #ROWB/4-1,d0
.fz:    clr.l (a0)+
        dbra d0,.fz

        ; ---- build copper list at $60000 ----
        lea CLIST,a1
        move.l #$01006600,(a1)+   ; BPLCON0: 6 planes, dual playfield, lores
        move.l #$01020062,(a1)+   ; BPLCON1: PF2H=6 PF1H=2 (game value)
        move.l #$0108ffd8,(a1)+   ; BPL1MOD = -ROWB (static rows)
        move.l #$010affd8,(a1)+   ; BPL2MOD = -ROWB
        move.l #$01800000,(a1)+   ; COLOR00 black
        move.l #$01820fff,(a1)+   ; COLOR01 white (PF1 window reference)
        move.l #$00920038,(a1)+   ; DDFSTRT $38 (game)
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0 (game, 20-word overscan row)
        move.l #$008e2c90,(a1)+   ; DIWSTRT: v44 h$90 (game)
        move.l #$0090f4b0,(a1)+   ; DIWSTOP: v244 h$1B0 (game)
        move.l #$00e00004,(a1)+   ; BPL1PT -> SOLID
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT -> ZERO
        move.l #$00e60100,(a1)+
        move.l #$00e80004,(a1)+   ; BPL3PT -> ZERO
        move.l #$00ea0100,(a1)+
        move.l #$00ec0004,(a1)+   ; BPL4PT -> ZERO
        move.l #$00ee0100,(a1)+
        move.l #$00f00004,(a1)+   ; BPL5PT -> ZERO
        move.l #$00f20100,(a1)+
        move.l #$00f40004,(a1)+   ; BPL6PT -> ZERO
        move.l #$00f60100,(a1)+

        move.w #$30,d1            ; first block (inside DIW top v44)
.block:
        ; -- 16 reference lines: solid plane-1 row, COLOR00 stays black --
        move.w d1,d2
        lsl.w  #8,d2
        addq.w #1,d2              ; WAIT(V,$00)
        move.w d2,(a1)+
        move.w #$ff00,(a1)+       ; vertical-only compare
        move.l #$00e00004,(a1)+   ; BPL1PT -> SOLID
        move.l #$00e20000,(a1)+
        move.l #$01800000,(a1)+   ; COLOR00 black

        ; -- 16 SotB lines: zero row, per-line COLOR00 on/off toggles --
        move.w d1,d2
        add.w  #16,d2
        lsl.w  #8,d2
        addq.w #1,d2
        move.w d2,(a1)+
        move.w #$ff00,(a1)+
        move.l #$00e00004,(a1)+   ; BPL1PT -> ZERO
        move.l #$00e20100,(a1)+

        move.w d1,d3
        add.w  #16,d3             ; first toggled line
        moveq  #16-1,d4
.line:
        move.w d3,d2
        lsl.w  #8,d2
        or.w   #$41,d2            ; WAIT(V,$40) full compare, like SotB
        move.w d2,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01800678,(a1)+   ; COLOR00 = band blue
        move.w d3,d2
        lsl.w  #8,d2
        or.w   #$d1,d2            ; WAIT(V,$D0)
        move.w d2,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01800000,(a1)+   ; COLOR00 = black
        addq.w #1,d3
        dbra   d4,.line

        add.w  #32,d1
        cmp.w  #$f0,d1
        bne    .block
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
