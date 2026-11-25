; Line-mode BZERO after a texture-counter wrap: X-major versus Y-major.
;
; Each case runs one 88-pixel line blit with a zero texture (BLTBDAT=0,
; USEB clear), A=$8000, LF=$CA (D = A&B | ~A&C) over a cleared bitplane, so
; no pixel writes a non-zero word. The CPU then reads DMACONR and draws a
; bar: full width (20 words) when BZERO is set, one word when it is clear.
;
;   rows 16-23: BLTCON1 $0051  SUD|SIGN, X-major, USEC       -> full
;   rows 32-39: BLTCON1 $0051  SUD|SIGN, X-major, USEC clear -> full
;   rows 48-55: BLTCON1 $0053  as row 1 with SING            -> full
;   rows 64-71: BLTCON1 $0041  SIGN, Y-major, USEC           -> short
;
; The error term starts negative (BLTAPT -100) and BLTBMOD is 0, so it stays
; negative and the line never takes its "sometimes" step: the X-major lines
; step only in X and never change row, the Y-major line steps in Y on every
; pixel. The register-texture BZERO evaluation that follows each BSH wrap
; only happens after a step to a new row, so only the Y-major line clears
; BZERO. Hardware references: vAmigaTS Agnus/Blitter/line/zero1 (the first
; three rows' blits, A500 ECS photo: BZERO set) and the One ROM A500 wrap
; captures recorded in docs/internals/a500-line-bzero-2026-10-15.md (Y-major
; lines: BZERO cleared after the first wrap). Regression class: the wrap
; evaluation applied to every line cleared BZERO for zero1's X-major lines.
CUST   equ $dff000
BMP    equ $40000
LDST   equ $50000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0                ; clear the display and the line target
        move.w #(LDST+$4000-BMP)/4-1,d0
.cz:    clr.l (a0)+
        dbra d0,.cz
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83c0,$96(a6)     ; SET DMAEN|BPLEN|COPEN|BLTEN

        move.w #$0bca,d4          ; USEA|USEC|USED, LF=$CA
        move.w #$0051,d5
        moveq #16,d2
        bsr.w case
        move.w #$09ca,d4          ; USEA|USED, LF=$CA
        move.w #$0051,d5
        moveq #32,d2
        bsr.w case
        move.w #$0bca,d4
        move.w #$0053,d5
        moveq #48,d2
        bsr.w case
        move.w #$0bca,d4
        move.w #$0041,d5
        moveq #64,d2
        bsr.w case
.halt:  bra.s .halt

; d4 = BLTCON0, d5 = BLTCON1, d2 = bar row.
case:   bsr.w bwait
        move.w #40,$60(a6)        ; BLTCMOD
        move.w #40,$66(a6)        ; BLTDMOD
        move.w #-100,$52(a6)      ; BLTAPTL: error term
        move.w #-200,$64(a6)      ; BLTAMOD
        clr.w $62(a6)             ; BLTBMOD
        move.w #$8000,$74(a6)     ; BLTADAT
        move.l #$ffffffff,$44(a6) ; BLTAFWM/BLTALWM
        clr.w $72(a6)             ; BLTBDAT: zero texture
        clr.w $70(a6)             ; BLTCDAT
        move.l #LDST+$800+18,$48(a6) ; BLTCPT
        move.l #LDST+$800+18,$54(a6) ; BLTDPT
        move.w d4,$40(a6)
        move.w d5,$42(a6)
        move.w #(88<<6)|2,$58(a6) ; BLTSIZE: 88 pixels
        bsr.w bwait
        moveq #1,d3
        btst #5,$02(a6)           ; DMACONR bit 13 (high byte bit 5): BZERO
        beq.s .draw
        moveq #20,d3
.draw:  bra.w drawbar

bwait:  tst.w $02(a6)
.w:     btst #6,$02(a6)
        bne.s .w
        rts

; Draw a bar: d2 = first bitmap row, d3 = width in words, 8 rows tall.
drawbar:
        lea BMP,a0
        move.w d2,d0
        mulu #40,d0
        adda.l d0,a0
        moveq #8-1,d1
.row:   move.l a0,a2
        move.w d3,d0
        subq.w #1,d0
.wds:   move.w #$ffff,(a2)+
        dbra d0,.wds
        lea 40(a0),a0
        dbra d1,.row
        rts
