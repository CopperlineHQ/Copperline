; Sprite display-latch disarm-before-HSTART probe.
;
; A sprite DMA data fetch lands in Denise's SPRxDATA/SPRxDATB latches and
; arms the channel, but the serializer only copies those latches when the
; horizontal comparator fires at HSTART. A SPRxCTL write clears the armed
; bit, so a CTL write that lands after the fetch slot but BEFORE HSTART
; cancels the fetched line completely -- nothing is ever loaded for it --
; while a CTL write after HSTART cannot recall pixels the serializer has
; already shifted out.
;
; Regression example: Hybris draws its SCORE/LIVES/HIGH panel with two
; Copper-multiplexed DMA sprites and retires them on the line below the
; panel with SPRxCTL writes at the very start of that line. Sprite DMA is
; still fetching there (the descriptor's vertical stop never matches), so
; the fetch re-arms the channel; without the disarm the fetched words paint
; a stray 16-px dash under the panel digits (issue #278).
;
; Every intervention writes the SAME CTL value the descriptor itself
; carries ($B000: vstop=$B0, no ATT, no HSTART bit 0, no VSTART bit 8), so
; the vertical DMA window is unchanged and the only effect under test is
; the disarm.
;
;   SPR0 (colour 17, red): DMA bar v=$40..$B0 at h-byte $70 (HSTART=$E0,
;     colour clock $70). Copper interventions:
;       v=$50..$5F  SPR0CTL at colour clock $30 -- after the $15/$17 fetch
;                   slots, before HSTART: these 16 lines MUST be blank.
;       v=$70..$7F  SPR0CTL at colour clock $A0 -- past HSTART: the bar
;                   MUST still be there.
;   SPR2 (colour 23, green): DMA bar v=$40..$B0 at h-byte $88 (HSTART=$110,
;     colour clock $88). Copper interventions:
;       v=$90..$9F  SPR2CTL disarm at colour clock $30 followed by
;                   SPRxPOS reposition to h-byte $A0. The disarm sticks
;                   across the POS write (POS never arms), so these lines
;                   MUST be blank at BOTH h-bytes. Painting the fetched
;                   words at the repositioned HSTART is the pre-fix
;                   regression -- the DMA-loaded data reuse path ignoring
;                   the disarm.
;
; Expected settled render: the 16-px white ruler, a red bar at x=$E0 from
; v=$40 to $B0 with a 16-line gap at v=$50, a green bar at x=$110 over the
; same span with a 16-line gap at v=$90, and nothing at x=$140.
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh).
CUST   equ $dff000
RULER  equ $40000
DESC0  equ $48000
DESC2  equ $48200
TERM   equ $48500
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

        ; ---- SPR0: bar v=$40..$B0 at h-byte $70, then terminator ----
        lea DESC0,a0
        move.w #$4070,(a0)+       ; POS  v=$40 h-byte=$70 (HSTART=$E0)
        move.w #$b000,(a0)+       ; CTL  vstop=$B0
        move.w #$b0-$40-1,d0
.s0:    move.w #$ffff,(a0)+       ; DATA -> colour 17
        move.w #$0000,(a0)+       ; DATB
        dbra d0,.s0
        clr.w (a0)+
        clr.w (a0)+

        ; ---- SPR2: bar v=$40..$B0 at h-byte $88, then terminator ----
        lea DESC2,a0
        move.w #$4088,(a0)+       ; POS  v=$40 h-byte=$88 (HSTART=$110)
        move.w #$b000,(a0)+       ; CTL  vstop=$B0
        move.w #$b0-$40-1,d0
.s2:    move.w #$ffff,(a0)+       ; DATA -> colour 23 with DATB set
        move.w #$ffff,(a0)+       ; DATB
        dbra d0,.s2
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
        move.l #$01a20f00,(a1)+   ; COLOR17 red   (SPR0 bar)
        move.l #$01a400f0,(a1)+   ; COLOR18 green
        move.l #$01a6000f,(a1)+   ; COLOR19 blue
        move.l #$01aa0f0f,(a1)+   ; COLOR21 magenta
        move.l #$01ac00ff,(a1)+   ; COLOR22 cyan
        move.l #$01ae00f0,(a1)+   ; COLOR23 green (SPR2 bar)
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$01200004,(a1)+   ; SPR0PT = DESC0
        move.l #$01228000,(a1)+
        move.l #$01240004,(a1)+   ; SPR1PT = TERM
        move.l #$01268500,(a1)+
        move.l #$01280004,(a1)+   ; SPR2PT = DESC2
        move.l #$012a8200,(a1)+
        move.l #$012c0004,(a1)+   ; SPR3PT = TERM
        move.l #$012e8500,(a1)+
        move.l #$01300004,(a1)+   ; SPR4PT = TERM
        move.l #$01328500,(a1)+
        move.l #$01340004,(a1)+   ; SPR5PT = TERM
        move.l #$01368500,(a1)+
        move.l #$01380004,(a1)+   ; SPR6PT = TERM
        move.l #$013a8500,(a1)+
        move.l #$013c0004,(a1)+   ; SPR7PT = TERM
        move.l #$013e8500,(a1)+

        ; v=$50..$5F: disarm SPR0 at colour clock $30 (before HSTART=$E0)
        move.w #$5031,d0          ; WAIT vp=$50 hp=$30
        moveq #16-1,d1
.early0:
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$0142b000,(a1)+   ; SPR0CTL = $B000 (disarms only)
        add.w #$0100,d0
        dbra d1,.early0

        ; v=$70..$7F: disarm SPR0 at colour clock $A0 (past HSTART=$E0)
        move.w #$70a1,d0          ; WAIT vp=$70 hp=$A0
        moveq #16-1,d1
.late0:
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$0142b000,(a1)+   ; SPR0CTL = $B000
        add.w #$0100,d0
        dbra d1,.late0

        ; v=$90..$9F: disarm SPR2 at colour clock $30, then reposition it
        move.w #$9031,d0          ; WAIT vp=$90 hp=$30
        moveq #16-1,d1
.early2:
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$0152b000,(a1)+   ; SPR2CTL = $B000 (disarms)
        move.l #$015040a0,(a1)+   ; SPR2POS -> h-byte $A0 (never arms)
        add.w #$0100,d0
        dbra d1,.early2

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l
