; Sprite display-latch DMA write-through probe.
;
; Sprite DMA fetches land in the same SPRxPOS/CTL/DATA/DATB registers a
; CPU/Copper write hits: a DMA DATA fetch arms the Denise display latch and
; a DMA CTL fetch (at vstop, including the 0/0 list terminator) disarms it
; and leaves the fetched control words in the registers. Software relies on
; the terminator's CTL to silence a channel for good: arming it again later
; with a bare SPRxDATA write redisplays whatever the registers then hold
; (the DMA-written words), not the last manually written pattern.
; Regression example: Hamazing's kaleidoscope scene writes SPRxDATA=$0000
; after a DMA sprite scene and expects invisible sprites; stale manual
; latches from an earlier scene would paint full-height bars.
;
; Timeline (manual arms write POS, CTL, DATB, DATA in that order -- the
; CTL write programs the vertical window and disarms, the DATA write arms
; last; every arm uses a display-covering window v=$28..$130, the gen-x
; masking-bar pattern):
;   Phase A (75 frames, SPREN off): manually arm SPR0/2/3/6 as full-height
;     $FFFF/$FFFF bars at h-bytes $88/$98/$A8/$B8.
;   Phase B (75 frames, SPREN on): SPR0 runs a DMA descriptor (bar
;     v=$50..$60 at h-byte $90, lines DATA=$FFFF DATB=$0000, then the 0/0
;     terminator); SPR1..7 park on the terminator. Every channel's latch is
;     disarmed by the terminator CTL fetch each frame; SPR0's DATA/DATB
;     hold the last DMA words ($FFFF/$0000).
;   Phase C (steady state, SPREN off):
;     SPR0: POS/CTL window at h-byte $D8, then SPRxDATA=$0000 -- the
;       arm-with-zero idiom. Must stay invisible: both data latches hold
;       DMA-written zeros after the phase-B fetches ($0000 was the last
;       DATB word and the manual DATA write is zero). A bar at $D8 means
;       the DMA data words missed the latch.
;     SPR2: full manual re-arm at h-byte $C8 (DATA=$FF00 DATB=$00FF): a
;       half-colour-1 half-colour-2 bar MUST appear (manual arm still works
;       after a DMA disarm).
;     SPR6: POS/CTL window at h-byte $D0 and SPRxDATA=$0F0F, no DATB
;       write: a bar MUST appear, striped colours 2/3 -- the DATB latch
;       still holds phase-A $FFFF because SPR6 only ever fetched null
;       control words (CTL touches the armed flag and the control
;       registers, never the data latches).
;     SPR3: untouched -- stays invisible (disarmed by the phase-B
;       terminator fetch). A bar at $A8 is the pre-fix regression: sprite
;       DMA never landing in the latch view at all.
;
; Expected steady render: the 16-px ruler plus exactly two full-height
; bars (SPR2 at $C8, SPR6 at $D0). A surviving phase-A bar means the
; terminator CTL fetch did not disarm the latch; a bar at $D8 means the
; DMA DATA/DATB words did not land in the latch.
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh), which models DMA
; sprite fetches through the same register path.
CUST   equ $dff000
RULER  equ $40000
DESC0  equ $48000
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

        ; ---- SPR0 DMA descriptor: bar v=$50..$60, then 0/0 terminator ----
        lea DESC0,a0
        move.w #$5090,(a0)+       ; POS  v=$50 h-byte=$90
        move.w #$6000,(a0)+       ; CTL  vstop=$60
        moveq #16-1,d0
.s0:    move.w #$ffff,(a0)+       ; DATA
        move.w #$0000,(a0)+       ; DATB: the DMA zeroes the B latch
        dbra d0,.s0
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
        move.l #$01280004,(a1)+   ; SPR2PT = TERM
        move.l #$012a8300,(a1)+
        move.l #$012c0004,(a1)+   ; SPR3PT = TERM
        move.l #$012e8300,(a1)+
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
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN (sprite DMA off)

        ; ---- phase A: manual full-height bars on SPR0/2/3/6 ----
        ; POS v=$28, CTL vstop=$130 ($30 + bit1): the display-covering
        ; window; CTL first (disarms), DATA last (arms).
        move.w #$2888,$140(a6)    ; SPR0POS
        move.w #$3002,$142(a6)    ; SPR0CTL
        move.w #$ffff,$146(a6)    ; SPR0DATB
        move.w #$ffff,$144(a6)    ; SPR0DATA (arms)
        move.w #$2898,$150(a6)    ; SPR2
        move.w #$3002,$152(a6)
        move.w #$ffff,$156(a6)
        move.w #$ffff,$154(a6)
        move.w #$28a8,$158(a6)    ; SPR3
        move.w #$3002,$15a(a6)
        move.w #$ffff,$15e(a6)
        move.w #$ffff,$15c(a6)
        move.w #$28b8,$170(a6)    ; SPR6
        move.w #$3002,$172(a6)
        move.w #$ffff,$176(a6)
        move.w #$ffff,$174(a6)
        move.w #75-1,d7
        bsr.w frames

        ; ---- phase B: sprite DMA on ----
        ; Toggle SPREN on line $136, after the last display line and clear
        ; of the frame wrap, so the next field starts with the new DMACON
        ; from its first line (real programs flip it in the vblank window;
        ; keeping the probe off that edge keeps it pinned to the latch
        ; behaviour, not DMACON-edge timing).
        bsr.w  line310
        move.w #$8020,$96(a6)
        move.w #75-1,d7
        bsr.w frames

        ; ---- phase C: SPREN off, manual writes, steady state ----
        bsr.w  line310
        move.w #$0020,$96(a6)
        move.w #$28d8,$140(a6)    ; SPR0: window at $D8, then arm-with-zero
        move.w #$3002,$142(a6)
        move.w #$0000,$144(a6)    ;   (must stay invisible: A=B=0 from DMA)
        move.w #$28c8,$150(a6)    ; SPR2: full manual re-arm (bar appears)
        move.w #$3002,$152(a6)
        move.w #$00ff,$156(a6)
        move.w #$ff00,$154(a6)
        move.w #$28d0,$170(a6)    ; SPR6: window + DATA only (striped bar:
        move.w #$3002,$172(a6)    ;   DATB latch still holds phase-A $FFFF)
        move.w #$0f0f,$174(a6)
.halt:  bra.s .halt

; Wait d7+1 frames: VPOSR V8 rise then fall per frame.
frames:
.r:     move.l $04(a6),d0
        btst   #16,d0
        beq.s  .r
.f:     move.l $04(a6),d0
        btst   #16,d0
        bne.s  .f
        dbra d7,frames
        rts

; Spin until the beam reaches line $136 (V8 set, V7-0 = $36): past the
; last display line, before the frame wrap.
line310:
.w:     move.l $04(a6),d0
        btst   #16,d0
        beq.s  .w
        lsr.w  #8,d0
        cmp.b  #$36,d0
        bne.s  .w
        rts
