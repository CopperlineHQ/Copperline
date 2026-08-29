; Empty vertical display window probe (DIWSTRT.V == DIWSTOP.V, whole frame).
;
; When DIWSTRT.V and DIWSTOP.V address the same line, the vertical display
; flop's set and reset comparators both match there and reset wins the tie:
; the window never opens and no bitplane is fetched or displayed anywhere in
; the frame. AGA software blanks the screen this way while it redraws
; buffers, parking both comparators on one line below the visible area via
; DIWHIGH's high bits (this probe uses line 301, DIWSTRT=$2D81 DIWSTOP=$2DC1
; DIWHIGH=$2101, the exact programming Nexus 7 uses between scenes).
;
; The vdiwprobe-flop tie band pins this for a mid-frame tie, but there the
; frame still fetches other bands, and the renderer's DMA-capture authority
; blanks the uncaptured rows. A frame whose window never opens captures NO
; rows at all, which routes every row onto the synthesized register-derived
; re-fetch path -- the level window test there read the empty window as a
; full wrap-around one and lit the whole frame with buffer contents the
; hardware never displayed (the Nexus 7 scene-transition corrupted-ball
; regression class).
;
; Sprites have no bitplane vertical comparator and BPLCON3.BRDRSPRT lets
; them display over the border, so they must stay visible over the closed
; frame (Nexus 7 keeps its spiral backdrop, 8 scan-doubled sprites, on
; screen this way while the bitplanes are blanked).
;
;   Bitplane: one plane of 8-on/8-off stripes, DDF armed, BPLEN on --
;     none of it may reach the screen.
;   SPR0 (colour 17, red): DMA bar v=$40..$B0 at h-byte $70.
;   SPR2 (colour 23, green): DMA bar v=$40..$B0 at h-byte $88.
;
; Expected settled render: dark blue border everywhere, a red bar at
; x=$E0 and a green bar at x=$110 over v=$40..$B0, and no stripe pixels
; anywhere.
CUST    equ $dff000
PLANE   equ $40000
DESC0   equ $48000
DESC2   equ $48200
TERM    equ $48500
CLIST   equ $60000
FILLW   equ 8192

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        lea PLANE,a0
        move.w #FILLW-1,d0
.fp:    move.w #$ff00,(a0)+       ; 8-on/8-off stripes
        dbra d0,.fp

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
        move.l #$01001201,(a1)+   ; BPLCON0: 1 plane, COLOR, ECSENA
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: sprites in front of the field
        move.l #$01060002,(a1)+   ; BPLCON3: BRDRSPRT, border not blanked
        move.l #$010c0011,(a1)+   ; BPLCON4: reset sprite banks
        move.l #$01fc0000,(a1)+   ; FMODE: OCS-compatible fetch/sprites
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2d81,(a1)+   ; DIWSTRT (resets DIWHIGH; rewritten below)
        move.l #$00902dc1,(a1)+   ; DIWSTOP
        move.l #$01e42101,(a1)+   ; DIWHIGH: vstart = vstop = 301
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE
        move.l #$00e20000,(a1)+
        move.l #$01800113,(a1)+   ; COLOR00 dark blue border
        move.l #$01820fff,(a1)+   ; COLOR01 white stripes (must not show)
        move.l #$01a20f00,(a1)+   ; COLOR17 red (SPR0)
        move.l #$01ae00f0,(a1)+   ; COLOR23 green (SPR2)
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l
