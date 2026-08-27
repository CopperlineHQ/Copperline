; AGA eight-plane dual-playfield decode and sprite priority probe.
;
; Lisa's dual playfield extends each field's colour index to four bits:
; PF1 = planes 1/3/5/7, PF2 = planes 2/4/6/8 (the Zool AGA 4-bit decode
; regression class -- a 3-bit decode drops planes 7/8 and reads the wrong
; palette entries). PF2's index is offset by the BPLCON3 PF2OF code
; (0,2,4,8,16,32,64,128); this probe programs code 6 (offset 64), so PF2
; pixels read the banked palette entries 64+, exercising BPLCON3 BANK
; writes as well. Sprite-vs-playfield priority follows the same
; winning-field rule as OCS/ECS (see sprprobe-dpfpri); every BPLCON2 code
; used here is in range, since Lisa's out-of-range colour behaviour
; deliberately diverges from vAmiga's model (see dual_playfield_pixel in
; src/video/bitplane/output.rs) and would invalidate the cross-check.
;
; Display: 8-plane lo-res FMODE=0 dual playfield whose pattern repeats a
; 4-px column cycle across every 16-px word:
;   px 0-3   background        (both fields transparent)
;   px 4-7   PF1 only, index 9 (planes 1+7 -> colour 9, red; a 3-bit
;                               decode reads colour 1, magenta)
;   px 8-11  PF2 only, index 9 (planes 2+8 -> colour 64+9=73, blue; a
;                               3-bit decode reads colour 65, green)
;   px 12-15 both opaque       (PF2PRI=0 shows PF1 red, PF2PRI=1 PF2 blue)
; Four unattached DMA sprite bars span v=$40..$E8, one per pair:
;   SPR0 white  HSTART=$0C0    SPR2 orange HSTART=$100
;   SPR4 yellow HSTART=$140    SPR6 cyan   HSTART=$180
; The Copper steps BPLCON2 through the same 24-line priority-code bands
; as sprprobe-dpfpri ($0020 / $0004 / $0000 / $0024 / $0012 / $000B /
; $0060 from v=$40), and each band renders the same sprite-visibility
; truth table with the both-opaque column following the winning field's
; code.
;
; Cross-checked against vAmiga 5 (tools/vamiga-ref.sh, A1200_2MB).
CUST    equ $dff000
PAT1    equ $40000
PAT2    equ $43000
PATZ    equ $46000
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
        lea PAT1,a0               ; planes 1+7: PF1 index 9 in px 4-7/12-15
        move.w #ROWS*20-1,d0
.f1:    move.w #$0f0f,(a0)+
        dbra d0,.f1
        lea PAT2,a0               ; planes 2+8: PF2 index 9 in px 8-15
        move.w #ROWS*20-1,d0
.f2:    move.w #$00ff,(a0)+
        dbra d0,.f2
        lea PATZ,a0               ; shared zero plane for planes 3-6
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
        move.l #$01000610,(a1)+   ; BPLCON0: 8 planes (BPU3), dual playfield
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: sprites in front until $40
        move.l #$010c0011,(a1)+   ; BPLCON4: default sprite colour banks
        move.l #$01fc0000,(a1)+   ; FMODE = 0
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01061800,(a1)+   ; BPLCON3: bank 0, PF2OF=6 (offset 64)
        move.l #$01800222,(a1)+   ; COLOR00 dark grey background
        move.l #$01820f0f,(a1)+   ; COLOR01 magenta: 3-bit PF1 decode tell
        move.l #$01920a00,(a1)+   ; COLOR09 PF1 red
        move.l #$01a20fff,(a1)+   ; COLOR17 SPR0 white
        move.l #$01aa0f80,(a1)+   ; COLOR21 SPR2 orange
        move.l #$01b20ff0,(a1)+   ; COLOR25 SPR4 yellow
        move.l #$01ba00ff,(a1)+   ; COLOR29 SPR6 cyan
        move.l #$01065800,(a1)+   ; BPLCON3: bank 2 (entries 64-95)
        move.l #$018200f0,(a1)+   ; entry 65 green: 3-bit PF2 decode tell
        move.l #$0192000a,(a1)+   ; entry 73 PF2 blue (index 9 + offset 64)
        move.l #$01061800,(a1)+   ; BPLCON3 back to bank 0, PF2OF=6
        move.l #$00e00004,(a1)+   ; BPL1PT = PAT1
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = PAT2
        move.l #$00e63000,(a1)+
        move.l #$00e80004,(a1)+   ; BPL3PT = PATZ
        move.l #$00ea6000,(a1)+
        move.l #$00ec0004,(a1)+   ; BPL4PT = PATZ
        move.l #$00ee6000,(a1)+
        move.l #$00f00004,(a1)+   ; BPL5PT = PATZ
        move.l #$00f26000,(a1)+
        move.l #$00f40004,(a1)+   ; BPL6PT = PATZ
        move.l #$00f66000,(a1)+
        move.l #$00f80004,(a1)+   ; BPL7PT = PAT1
        move.l #$00fa0000,(a1)+
        move.l #$00fc0004,(a1)+   ; BPL8PT = PAT2
        move.l #$00fe3000,(a1)+
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

        ; BPLCON2 priority-code bands (the sprprobe-dpfpri table)
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
