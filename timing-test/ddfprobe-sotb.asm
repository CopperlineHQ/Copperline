; Copper COLOR00 write landing vs the DIW border transition (Shadow of the
; Beast title band).
;
; The SotB title sequence paints its top parallax band's sky by toggling
; COLOR00 per scanline with the copper: WAIT (v,$40) MOVE COLOR00=$0678,
; WAIT (v,$D0) MOVE COLOR00=$0000, inside a narrow display window
; DIWSTRT.H=$81 / DIWSTOP.H=$A1 (right edge hires $1A1 = cck 208.5). The
; author placed the waits so both landings tuck against the window edges:
; the blue must fill the window's colour-0 pixels edge to edge, with no
; blue border sliver on the left and no black tail inside the right edge.
;
; Earlier probes calibrated raced COLOR00 trains against FETCHED BITPLANE
; pixels (ddfprobe-cc4) and the DIW edge against bitplane content
; (ddfprobe-diw1), but never a lone post-WAIT COLOR00 landing against the
; BORDER transition itself. This probe isolates exactly that: 16-line
; reference bands of solid colour-1 (window edge anchor) alternate with
; 16-line SotB bands (zeroed bitplane, per-line COLOR00 on/off toggles at
; the SotB wait positions). Measure the blue band's left/right edges
; against the white reference band's edges on Copperline vs vAmiga.
CUST   equ $dff000
SOLID  equ $40000
ZERO   equ $40100
CLIST  equ $60000
ROWB   equ 36                    ; 18 fetched words = 36 bytes per row

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
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, COLOR ON, lores
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$0108ffdc,(a1)+   ; BPL1MOD = -ROWB (static row)
        move.l #$010affdc,(a1)+   ; BPL2MOD = -ROWB
        move.l #$01800000,(a1)+   ; COLOR00 black
        move.l #$01820fff,(a1)+   ; COLOR01 white (window reference)
        move.l #$00920038,(a1)+   ; DDFSTRT $38 (18-word row)
        move.l #$009400c0,(a1)+   ; DDFSTOP $C0
        move.l #$008e2c81,(a1)+   ; DIWSTRT: v44 h$81
        move.l #$00902ca1,(a1)+   ; DIWSTOP: v300 h$1A1 (SotB narrow window)
        move.l #$00e00004,(a1)+   ; BPL1PTH ($40000 solid)
        move.l #$00e20000,(a1)+

        move.w #$30,d1            ; first block (inside DIW top v44)
.block:
        ; -- 16 reference lines: solid colour-1 row, COLOR00 stays black --
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
