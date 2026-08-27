; Dual-playfield sprite priority probe: Denise resolves the two playfields
; against each other first (opacity, then PF2PRI where both are opaque) and
; tests a sprite pixel against the WINNING field's BPLCON2 code only.
;
; BPLCON2 places each playfield on the sprite-pair chain (PF1P bits 2-0
; against the odd planes, PF2P bits 5-3 against the even planes). At a pixel
; where only one field is opaque, the sprite is tested against that field's
; code. Where BOTH are opaque, the field that wins the playfield comparison
; (PF2PRI, bit 6) carries its code into the sprite comparison and the losing
; field's code is ignored -- observable in the circular programmings where
; PF2PRI and the sprite codes disagree about the field order (e.g. $0004:
; PF1 beats PF2, PF2 beats the sprites, the sprites beat PF1; the winner is
; PF1 and its code 4 shows the sprite). vAmiga models the same rule with a
; single per-pixel playfield depth value.
;
; Regression example: Chuck Rock 2 (Core) draws its player as attached
; sprites over a 6-plane dual playfield with BPLCON2=$0020 (PF1P=0, PF2P=4):
; PF1 foreground bins hide the player while the player covers the PF2
; backdrop. Resolving the both-opaque columns against PF2P alone drew the
; player in front of the bins.
;
; Display: 6-plane lo-res dual playfield whose pattern repeats a 4-px column
; cycle across every 16-px word:
;   px 0-3   background      (both fields transparent)
;   px 4-7   PF1 only opaque (colour 1, red)
;   px 8-11  PF2 only opaque (colour 9, blue)
;   px 12-15 both opaque     (PF2PRI=0 shows PF1 red, PF2PRI=1 PF2 blue)
; Four unattached DMA sprite bars span v=$40..$E8, one per pair:
;   SPR0 white  HSTART=$0C0    SPR2 green  HSTART=$100
;   SPR4 yellow HSTART=$140    SPR6 cyan   HSTART=$180
; The Copper steps BPLCON2 through 24-line bands; within each band a bar is
; a comb whose teeth appear only over the columns the pair beats (the
; both-opaque column follows the winning field's code):
;
;   band v     BPLCON2  PF1P PF2P  bar visible over (per pair 0..3)
;   $40..$57   $0020     0    4    bg + PF2 columns (all pairs)  <- the
;              Chuck Rock 2 case: the both-opaque column hides the bar
;   $58..$6F   $0004     4    0    bg + PF1 + both columns (all pairs; the
;                                  circular case -- winner PF1's code 4
;                                  shows the bar over both-opaque)
;   $70..$87   $0000     0    0    bg column only   (all pairs)
;   $88..$9F   $0024     4    4    everywhere       (all pairs)
;   $A0..$B7   $0012     2    2    pairs 0-1 everywhere, pairs 2-3 bg only
;   $B8..$CF   $000B     3    1    pair 0 everywhere, pairs 1-2 bg + PF1 +
;                                  both (winner PF1's code 3), pair 3 bg only
;   $D0..$E7   $0060     0    4    PF2PRI=1: the both-opaque column flips
;                                  red -> blue and its winner PF2's code 4
;                                  shows every bar there, while PF1-only
;                                  columns still hide them
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh): byte-identical over
; the whole frame.
CUST    equ $dff000
PLANE1  equ $40000
PLANE2  equ $43000
PLANEZ  equ $46000
DESC0   equ $4a000
DESC2   equ $4a400
DESC4   equ $4a800
DESC6   equ $4ac00
TERM    equ $4b000
CLIST   equ $60000
ROWS    equ 256                   ; DIW v44..299
LINES   equ $e8-$40               ; sprite bar height

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        ; ---- playfield pattern planes ----
        lea PLANE1,a0             ; plane 1: PF1 opaque in px 4-7 / 12-15
        move.w #ROWS*20-1,d0
.f1:    move.w #$0f0f,(a0)+
        dbra d0,.f1
        lea PLANE2,a0             ; plane 2: PF2 opaque in px 8-15
        move.w #ROWS*20-1,d0
.f2:    move.w #$00ff,(a0)+
        dbra d0,.f2
        lea PLANEZ,a0             ; shared zero plane for planes 3-6
        move.w #ROWS*20-1,d0
.fz:    clr.w (a0)+
        dbra d0,.fz

        ; ---- sprite bars v=$40..$E8, one per pair ----
        lea DESC0,a0
        move.w #$4060,d1          ; POS v=$40 h-byte=$60 (HSTART=$0C0)
        bsr.s mkbar
        lea DESC2,a0
        move.w #$4080,d1          ; HSTART=$100
        bsr.s mkbar
        lea DESC4,a0
        move.w #$40a0,d1          ; HSTART=$140
        bsr.s mkbar
        lea DESC6,a0
        move.w #$40c0,d1          ; HSTART=$180
        bsr.s mkbar
        lea TERM,a0
        clr.w (a0)+
        clr.w (a0)+
        bra.s clist

mkbar:  move.w d1,(a0)+           ; POS
        move.w #$e800,(a0)+       ; CTL vstop=$E8
        move.w #LINES-1,d0
.mb:    move.w #$ffff,(a0)+       ; DATA -> colour 1 of the pair
        clr.w (a0)+               ; DATB
        dbra d0,.mb
        clr.w (a0)+               ; terminator
        clr.w (a0)+
        rts

        ; ---- copper list ----
clist:  lea CLIST,a1
        move.l #$01006600,(a1)+   ; BPLCON0: 6 planes, dual playfield
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: sprites in front until $40
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01800222,(a1)+   ; COLOR00 dark grey background
        move.l #$01820a00,(a1)+   ; COLOR01 PF1 red
        move.l #$0192000a,(a1)+   ; COLOR09 PF2 blue
        move.l #$01a20fff,(a1)+   ; COLOR17 SPR0 white
        move.l #$01aa00f0,(a1)+   ; COLOR21 SPR2 green
        move.l #$01b20ff0,(a1)+   ; COLOR25 SPR4 yellow
        move.l #$01ba00ff,(a1)+   ; COLOR29 SPR6 cyan
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE1 (PF1 pattern)
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = PLANE2 (PF2 pattern)
        move.l #$00e63000,(a1)+
        move.l #$00e80004,(a1)+   ; BPL3PT = PLANEZ
        move.l #$00ea6000,(a1)+
        move.l #$00ec0004,(a1)+   ; BPL4PT = PLANEZ
        move.l #$00ee6000,(a1)+
        move.l #$00f00004,(a1)+   ; BPL5PT = PLANEZ
        move.l #$00f26000,(a1)+
        move.l #$00f40004,(a1)+   ; BPL6PT = PLANEZ
        move.l #$00f66000,(a1)+
        move.l #$01200004,(a1)+   ; SPR0PT = DESC0
        move.l #$0122a000,(a1)+
        move.l #$01240004,(a1)+   ; SPR1PT = TERM
        move.l #$0126b000,(a1)+
        move.l #$01280004,(a1)+   ; SPR2PT = DESC2
        move.l #$012aa400,(a1)+
        move.l #$012c0004,(a1)+   ; SPR3PT = TERM
        move.l #$012eb000,(a1)+
        move.l #$01300004,(a1)+   ; SPR4PT = DESC4
        move.l #$0132a800,(a1)+
        move.l #$01340004,(a1)+   ; SPR5PT = TERM
        move.l #$0136b000,(a1)+
        move.l #$01380004,(a1)+   ; SPR6PT = DESC6
        move.l #$013aac00,(a1)+
        move.l #$013c0004,(a1)+   ; SPR7PT = TERM
        move.l #$013eb000,(a1)+

        ; BPLCON2 priority-code bands (see the table in the header)
        move.l #$40010020,d2
        bsr.s band                ; v=$40: $0020 (Chuck Rock 2 case)
        move.l #$58010004,d2
        bsr.s band                ; v=$58: $0004
        move.l #$70010000,d2
        bsr.s band                ; v=$70: $0000
        move.l #$88010024,d2
        bsr.s band                ; v=$88: $0024
        move.l #$a0010012,d2
        bsr.s band                ; v=$A0: $0012
        move.l #$b801000b,d2
        bsr.s band                ; v=$B8: $000B
        move.l #$d0010060,d2
        bsr.s band                ; v=$D0: $0060 (PF2PRI set)
        move.l #$e8010024,d2
        bsr.s band                ; v=$E8: tail below the bars

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l

band:   swap d2
        move.w d2,(a1)+           ; WAIT vp,hp=$01
        move.w #$fffe,(a1)+
        move.w #$0104,(a1)+       ; MOVE BPLCON2
        swap d2
        move.w d2,(a1)+
        rts
