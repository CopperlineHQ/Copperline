; AGA BPLCON4 sprite palette-bank probe (ESPRM/OSPRM split).
;
; Lisa reads sprite colours from a 16-entry palette bank selected per
; sprite parity by BPLCON4's low byte: EVEN sprites use ESPRM (bits 7-4)
; and ODD sprites use OSPRM (bits 3-0) as the high nibble of the palette
; index. An ATTACHED pair displays through the odd channel and takes the
; OSPRM bank for its 4-bit codes. The reset value $11 selects bank 1 for
; both, which is the classic 16..31 block -- so a swapped nibble decode
; (the Nexus 7 lamp-base regression class: even sprites reading the odd
; bank left the demo's fifth sprite on unset entries) is invisible until
; a title programs the nibbles apart.
;
; Seven sprite bars (SPR0..5 unattached, SPR6/7 attached) show three
; stripes (colour codes 1/2/3; the attached bar 3/7/11/15) while the
; Copper steps BPLCON4 through four 40-line bands:
;
;   band v      BPLCON4  even sprites        odd sprites + attached pair
;   $40..$67    $0011    classic 16+ (blue)  classic 16+ (blue)
;   $68..$8F    $0027    bank 2, 32+ (green) bank 7, 112+ (red)
;   $90..$B7    $0072    bank 7, 112+ (red)  bank 2, 32+ (green)
;   $B8..$DF    $00FF    bank 15, 240+ (wht) bank 15, 240+ (white)
;
; Palette families encode the bank in the hue and the entry in the ramp:
; classic 16+i = $2(i)F blue, 32+i = $2F(i) green, 112+i = $F(i)2 red,
; 240+i = $FF(i) white/yellow. In the $0027 band the correct decode shows
; green/red/green/red/green/red bars with a red attached bar; the swapped
; decode mirrors the unattached bars against the $0072 band while the
; attached bar stays put.
;
; Cross-checked against vAmiga 5.0b1 (tools/vamiga-ref.sh, A1200_2MB),
; whose Denise models the same esprm/osprm split: every band's colours
; map bijectively exactly. The one divergence is vertical: vAmiga draws
; these DMA bars one line low ($41..$E0 against the hardware-standard
; VSTART..VSTOP-1 = $40..$DF that agshres-sprites matches exactly), so an
; automated compare reports the bars' first and last lines only.
CUST    equ $dff000
BMP     equ $40000
DESC0   equ $48000
DESC1   equ $48400
DESC2   equ $48800
DESC3   equ $48c00
DESC4   equ $49000
DESC5   equ $49400
DESC6   equ $49800
DESC7   equ $49c00
CLIST   equ $60000
LINES   equ $e0-$40               ; sprite bar height

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        ; Seed the AGA selections before the Copper is enabled so the first
        ; active line is independent of where the bootstrap released us.
        move.w #$0000,$106(a6)   ; BPLCON3: bank 0, SPRES 00
        move.w #$0011,$10c(a6)   ; BPLCON4: classic sprite palette bank

        ; A zero plane keeps the whole display window at COLOR00 while its
        ; DMA supplies the BPL1DAT edge that gates ordinary sprite output.
        lea BMP,a0
        move.w #$3fff,d0
.fill:  clr.w (a0)+
        dbra d0,.fill

        ; ---- unattached bars: stripes of colour code 1 / 2 / 3 ----
        lea DESC0,a0
        move.w #$4060,d1          ; POS v=$40 h-byte=$60 (HSTART=$0C0)
        bsr.w mkbar
        lea DESC1,a0
        move.w #$4070,d1          ; HSTART=$0E0
        bsr.w mkbar
        lea DESC2,a0
        move.w #$4080,d1          ; HSTART=$100
        bsr.w mkbar
        lea DESC3,a0
        move.w #$4090,d1          ; HSTART=$120
        bsr.w mkbar
        lea DESC4,a0
        move.w #$40a0,d1          ; HSTART=$140
        bsr.w mkbar
        lea DESC5,a0
        move.w #$40b0,d1          ; HSTART=$160
        bsr.w mkbar

        ; ---- attached pair: SPR6 low code bits, SPR7 high bits + ATT ----
        lea DESC6,a0              ; codes 3/7/11/15 in 4-px stripes
        move.w #$40c0,(a0)+       ; POS v=$40 HSTART=$180
        move.w #$e000,(a0)+       ; CTL vstop=$E0
        move.w #LINES-1,d0
.at6:    move.w #$ffff,(a0)+       ; DATA: code bit 0 set everywhere
        move.w #$ffff,(a0)+       ; DATB: code bit 1 set everywhere
        dbra d0,.at6
        clr.w (a0)+
        clr.w (a0)+
        lea DESC7,a0
        move.w #$40c0,(a0)+       ; POS matches SPR6
        move.w #$e080,(a0)+       ; CTL vstop=$E0, ATT
        move.w #LINES-1,d0
.at7:    move.w #$0f0f,(a0)+       ; DATA: code bit 2 over stripes 2/4
        move.w #$00ff,(a0)+       ; DATB: code bit 3 over stripes 3/4
        dbra d0,.at7
        clr.w (a0)+
        clr.w (a0)+
        bra.s clist

mkbar:  move.w d1,(a0)+           ; POS
        move.w #$e000,(a0)+       ; CTL vstop=$E0
        move.w #LINES-1,d0
.mb:    move.w #$f81f,(a0)+       ; DATA: px 0-4 code 1, 5-10 code 2,
        move.w #$07ff,(a0)+       ; DATB: px 11-15 code 3
        dbra d0,.mb
        clr.w (a0)+               ; terminator
        clr.w (a0)+
        rts

        ; ---- copper list ----
clist:  lea CLIST,a1
        move.l #$01fc0000,(a1)+   ; FMODE: 16-bit fetches
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: sprites in front
        move.l #$010c0011,(a1)+   ; BPLCON4: classic sprite palette bank
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00e00004,(a1)+   ; BPL1PT = BMP
        move.l #$00e20000,(a1)+

        ; Palette banks, each hue naming its bank (see header). BPLCON3
        ; BANK bits route the COLORxx writes; LOCT stays clear so each
        ; write fills both 12-bit halves.
        move.l #$01060000,(a1)+   ; bank 0: classic 16+i = $2(i)F blue
        move.w #$01a0,d2
        move.w #$020f,d3
        moveq #16-1,d4
.b0:    move.w d2,(a1)+
        move.w d3,(a1)+
        addq.w #2,d2
        add.w #$0010,d3
        dbra d4,.b0
        move.l #$01800222,(a1)+   ; COLOR00 dark grey background
        move.l #$01062000,(a1)+   ; bank 1: entries 32+i = $2F(i) green
        move.w #$0180,d2
        move.w #$02f0,d3
        moveq #16-1,d4
.b1:    move.w d2,(a1)+
        move.w d3,(a1)+
        addq.w #2,d2
        addq.w #1,d3
        dbra d4,.b1
        move.l #$01066000,(a1)+   ; bank 3: entries 112+i = $F(i)2 red
        move.w #$01a0,d2
        move.w #$0f02,d3
        moveq #16-1,d4
.b3:    move.w d2,(a1)+
        move.w d3,(a1)+
        addq.w #2,d2
        add.w #$0010,d3
        dbra d4,.b3
        move.l #$0106e000,(a1)+   ; bank 7: entries 240+i = $FF(i) white
        move.w #$01a0,d2
        move.w #$0ff0,d3
        moveq #16-1,d4
.b7:    move.w d2,(a1)+
        move.w d3,(a1)+
        addq.w #2,d2
        addq.w #1,d3
        dbra d4,.b7
        move.l #$01060000,(a1)+   ; BPLCON3 back to bank 0

        ; Sprite pointers: all eight channels carry a bar.
        move.w #$0120,d2          ; SPR0PTH
        move.w #$8000,d3          ; DESC0 low word
        moveq #8-1,d4
.spt:   move.w d2,(a1)+
        move.w #$0004,(a1)+
        addq.w #2,d2
        move.w d2,(a1)+
        move.w d3,(a1)+
        addq.w #2,d2
        add.w #$0400,d3
        dbra d4,.spt

        ; BPLCON4 bands (see the table in the header). Each write lands at
        ; the very start of its band's first line, before that line's sprite
        ; output, so every band line renders one whole BPLCON4 value (and
        ; vAmiga's per-line sprite colouring agrees exactly).
        move.l #$6801fffe,(a1)+
        move.l #$010c0027,(a1)+   ; v=$68: ESPRM=2, OSPRM=7
        move.l #$9001fffe,(a1)+
        move.l #$010c0072,(a1)+   ; v=$90: ESPRM=7, OSPRM=2
        move.l #$b801fffe,(a1)+
        move.l #$010c00ff,(a1)+   ; v=$B8: both nibbles $F

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l
