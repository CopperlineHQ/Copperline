; Per-line BPLCON0-toggle placement probe.
;
; The gen-x rotozoom mosaic turns the bitplane display off and on every line
; (copper MOVEs BPLCON0=$0200 at hpos $E2, then $7200 at hpos $00 of the next
; line -- BPU 7<->0) instead of leaving it on. This probe reproduces that so we
; can see, against vAmiga, whether the per-line turn-on shifts the left edge
; relative to a statically-on display (the "wide left column" candidate).
;
; Top half (vpos $30..$7F): BPLCON0 = $7200 set once (statically on).
; Bottom half (vpos $80..$EF): the copper toggles BPLCON0 $7200 at hpos $00 and
;   $0200 at hpos $E0 EVERY line -- built at runtime into a copper list at
;   $60000.
; Both halves are BPU=7 lo-res, DDFSTRT $38 / DDFSTOP $D0, DIW wide-open. Plane 1
; is the $8000-per-word ruler; measure the first white line's x in each half on
; Copperline and vAmiga. If the toggle half's left edge moves (or the two
; emulators disagree on it), that is the gen-x mechanism.

CUST    equ     $dff000
RULER   equ     $40000
ZERO    equ     $50000
CLIST   equ     $60000
FILLW   equ     14336

;----------------------------------------------------------------- entry
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)  ; INTENA off
        move.w  #$7fff,$9c(a6)  ; INTREQ clear
        move.w  #$7fff,$96(a6)  ; DMACON off

        lea     RULER,a0
        move.w  #FILLW-1,d0
.fill:
        move.w  #$8000,(a0)+
        dbra    d0,.fill
        lea     ZERO,a0
        move.w  #FILLW-1,d0
.fz:
        clr.w   (a0)+
        dbra    d0,.fz

        ; ---- build the copper list at $60000 ----
        lea     CLIST,a1
        move.l  #$01020000,(a1)+        ; BPLCON1
        move.l  #$01080000,(a1)+        ; BPL1MOD
        move.l  #$010a0000,(a1)+        ; BPL2MOD
        move.l  #$01820fff,(a1)+        ; COLOR01 = white
        move.l  #$008e2c50,(a1)+        ; DIWSTRT wide-open
        move.l  #$00902cd0,(a1)+        ; DIWSTOP wide-open
        move.l  #$00920038,(a1)+        ; DDFSTRT $38
        move.l  #$009400d0,(a1)+        ; DDFSTOP $D0
        move.l  #$00e00004,(a1)+        ; BPL1PTH ($40000)
        move.l  #$00e20000,(a1)+        ; BPL1PTL
        move.l  #$00e40005,(a1)+        ; BPL2PT ($50000 zero)
        move.l  #$00e60000,(a1)+
        move.l  #$00e80005,(a1)+        ; BPL3
        move.l  #$00ea0000,(a1)+
        move.l  #$00ec0005,(a1)+        ; BPL4
        move.l  #$00ee0000,(a1)+
        move.l  #$00f00005,(a1)+        ; BPL5
        move.l  #$00f20000,(a1)+
        move.l  #$00f40005,(a1)+        ; BPL6
        move.l  #$00f60000,(a1)+

        move.l  #$01800002,(a1)+        ; COLOR00 = dark blue (static-on half)
        move.l  #$01007200,(a1)+        ; BPLCON0 = $7200 (statically on)
        move.l  #$8001ff00,(a1)+        ; WAIT vpos $80
        move.l  #$01800202,(a1)+        ; COLOR00 = purple (toggle half)

        move.w  #$80,d1                 ; first toggled line
.tloop:
        move.w  d1,d2
        lsl.w   #8,d2
        addq.w  #1,d2                   ; WAIT(V,$00): (V<<8)|1
        move.w  d2,(a1)+
        move.w  #$fffe,(a1)+
        move.l  #$01007200,(a1)+        ; MOVE BPLCON0 $7200 (on)
        move.w  d1,d2
        lsl.w   #8,d2
        ori.w   #$e1,d2                 ; WAIT(V,$E0): (V<<8)|$E1
        move.w  d2,(a1)+
        move.w  #$fffe,(a1)+
        move.l  #$01000200,(a1)+        ; MOVE BPLCON0 $0200 (off)
        addq.w  #1,d1
        cmp.w   #$f0,d1
        bne.s   .tloop

        move.l  #$fffffffe,(a1)+        ; end of copper list

        move.l  #CLIST,$80(a6)          ; COP1LC
        move.w  d0,$88(a6)              ; COPJMP1 strobe

        move.w  #$8380,$96(a6)          ; DMAEN | BPLEN | COPEN
.loop:
        bra.s   .loop
