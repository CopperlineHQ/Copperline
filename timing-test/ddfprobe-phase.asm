; DDFSTRT sub-unit phase placement probe (lo-res, FMODE=0).
; 15 bands of 8 lines walk DDFSTRT through $38-$42 and $60-$70 in steps of 2
; while the copper re-anchors BPL1PT to the same marker bitmap on every line
; (BPLCON1=0, mods 0). Each band therefore displays identical data through an
; identical pipeline except for the DDFSTRT phase, so the marker bar's X per
; band IS the phase->placement map: it answers whether placement is linear in
; DDFSTRT (4 px per step), quantized to the fetch grid with round-down, or
; quantized with round-up, and where the quantum boundary sits. Regression
; example: Rampage's dot-cube part pans by walking DDFSTRT $66->$68 against a
; BPLCON1 wrap, so a wrong boundary shows as 16px jumps a few times a second.
; Marker: $FFFF $F000 (16px bar, then a 4px tick) at the start of the row.
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000
NPHASE equ 15
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #$ffff,(a0)+
        move.w #$f000,(a0)+
        move.w #62-1,d0
.z:     clr.w (a0)+
        dbra d0,.z
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$009400c8,(a1)+   ; DDFSTOP $C8
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00920070,(a1)+   ; DDFSTRT preview above the bands
        move.l #$00e00004,(a1)+   ; BPL1PTH = $40000
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea phases(pc),a2
        moveq #NPHASE-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d4
        moveq #8-1,d5
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07)
        move.w #$fffe,(a1)+
        move.w #$0092,(a1)+       ; DDFSTRT = phase
        move.w d4,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w #$0000,(a1)+
        add.w #$0100,d3
        dbra d5,.line
        dbra d2,.band
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
        cnop 0,2
phases: dc.w $38,$3a,$3c,$3e,$40,$42,$60,$62,$64,$66,$68,$6a,$6c,$6e,$70
