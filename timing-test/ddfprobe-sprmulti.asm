; DMA sprite structure probe: vertical reuse and attachment.
;
; Over the 16-px ruler bitmap:
;   SPR0 (colours 17-19): one channel carrying TWO POS/CTL groups -- a bar
;        at v=$50..$60 x=$120, then the SAME channel reused at v=$70..$80
;        one lo-res px right (the sprite register FSM must rearm from the
;        in-stream control words), then the 0,0 terminator.
;   SPR2+SPR3 (colours 17-31): attached pair at v=$90..$A0 (SPR3 CTL bit 7);
;        the 4-bit combined index selects the upper palette bank -- an
;        attachment or priority regression changes the bar's colours.
; SPR1/4/5/6/7 park on a 0,0 terminator. All pointers are rewritten by the
; copper at the top of every frame, as hardware requires.
CUST   equ $dff000
RULER  equ $40000
DESC0  equ $48000
DESC2  equ $48100
DESC3  equ $48200
TERM   equ $48300
CLIST  equ $60000
FILLW  equ 14336

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea RULER,a0
        move.w #FILLW-1,d0
.fr:    move.w #$8000,(a0)+       ; 1 white px every 16 lo-res px
        dbra d0,.fr

        ; ---- SPR0: bar, reuse bar one px right, terminator ----
        lea DESC0,a0
        move.w #$5090,(a0)+       ; POS  v=$50 h-byte=$90 (x=$120)
        move.w #$6000,(a0)+       ; CTL  vstop=$60
        moveq #16-1,d0
.s0a:   move.w #$ffff,(a0)+       ; DATA
        move.w #$0f0f,(a0)+       ; DATB
        dbra d0,.s0a
        move.w #$7090,(a0)+       ; POS  v=$70, same h-byte
        move.w #$8001,(a0)+       ; CTL  vstop=$80, H0=1 (+1 lo-res px)
        moveq #16-1,d0
.s0b:   move.w #$ffff,(a0)+
        move.w #$0f0f,(a0)+
        dbra d0,.s0b
        clr.w (a0)+
        clr.w (a0)+

        ; ---- SPR2/SPR3: attached pair ----
        lea DESC2,a0
        move.w #$9090,(a0)+       ; POS  v=$90 x=$120
        move.w #$a000,(a0)+       ; CTL  vstop=$A0
        moveq #16-1,d0
.s2:    move.w #$ff00,(a0)+       ; DATA (low plane pair)
        move.w #$f0f0,(a0)+       ; DATB
        dbra d0,.s2
        clr.w (a0)+
        clr.w (a0)+
        lea DESC3,a0
        move.w #$9090,(a0)+       ; POS  identical position
        move.w #$a080,(a0)+       ; CTL  ATT set
        moveq #16-1,d0
.s3:    move.w #$0ff0,(a0)+       ; DATA (high plane pair)
        move.w #$cccc,(a0)+       ; DATB
        dbra d0,.s3
        clr.w (a0)+
        clr.w (a0)+

        lea TERM,a0
        clr.w (a0)+
        clr.w (a0)+

        ; ---- copper list ----
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white ruler
        move.l #$01a20f00,(a1)+   ; COLOR17 red
        move.l #$01a400f0,(a1)+   ; COLOR18 green
        move.l #$01a6000f,(a1)+   ; COLOR19 blue
        move.l #$01a80ff0,(a1)+   ; COLOR20 yellow
        move.l #$01aa0f0f,(a1)+   ; COLOR21 magenta
        move.l #$01ac00ff,(a1)+   ; COLOR22 cyan
        move.l #$01ae0fff,(a1)+   ; COLOR23 white
        move.l #$01b00800,(a1)+   ; COLOR24 dark red
        move.l #$01b20080,(a1)+   ; COLOR25 dark green
        move.l #$01b40008,(a1)+   ; COLOR26 dark blue
        move.l #$01b60880,(a1)+   ; COLOR27 olive
        move.l #$01b80808,(a1)+   ; COLOR28 purple
        move.l #$01ba0088,(a1)+   ; COLOR29 teal
        move.l #$01bc0888,(a1)+   ; COLOR30 grey
        move.l #$01be0fa5,(a1)+   ; COLOR31 orange
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$01200004,(a1)+   ; SPR0PTH
        move.l #$01228000,(a1)+   ; SPR0PTL = DESC0
        move.l #$01240004,(a1)+   ; SPR1PT = TERM
        move.l #$01268300,(a1)+
        move.l #$01280004,(a1)+   ; SPR2PT = DESC2
        move.l #$012a8100,(a1)+
        move.l #$012c0004,(a1)+   ; SPR3PT = DESC3
        move.l #$012e8200,(a1)+
        move.l #$01300004,(a1)+   ; SPR4PT = TERM
        move.l #$01328300,(a1)+
        move.l #$01340004,(a1)+   ; SPR5PT = TERM
        move.l #$01368300,(a1)+
        move.l #$01380004,(a1)+   ; SPR6PT = TERM
        move.l #$013a8300,(a1)+
        move.l #$013c0004,(a1)+   ; SPR7PT = TERM
        move.l #$013e8300,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l
