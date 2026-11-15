; An armed sprite has no vertical window of its own.
;
; Denise has no vertical comparator for sprites. SPRxPOS/SPRxCTL carry
; VSTART and VSTOP only for Agnus, which uses them to time the sprite DMA
; fetches; in Denise a SPRxDATA write arms the sprite, a SPRxCTL write
; disarms it, and an armed sprite is serialized at its horizontal position
; on every line, in the frame it was armed in and in every frame after it.
;
; The probe never enables sprite DMA. It arms three sprites once, each
; with control words that describe a different vertical window, and then
; leaves the sprite registers alone:
;
;   SPR0  h-byte $90, VSTART = VSTOP = 0 (an empty window), red
;   SPR2  h-byte $A8, VSTART $50, VSTOP $60 (sixteen lines), magenta
;   SPR4  h-byte $C0, VSTART $C0, VSTOP $40 (inverted), green
;
; Expected steady render: the 16-px ruler and three bars, sixteen lo-res
; pixels wide, each running the whole height of the display window. A
; missing SPR0 bar, a SPR2 bar sixteen lines tall or a SPR4 bar with a gap
; in the middle means the control words' vertical positions were applied
; to the display latch.
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh).
CUST   equ $dff000
RULER  equ $40000
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
        move.l #$01a20f00,(a1)+   ; COLOR17 red     (SPR0)
        move.l #$01aa0f0f,(a1)+   ; COLOR21 magenta (SPR2)
        move.l #$01b200f0,(a1)+   ; COLOR25 green   (SPR4)
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN (sprite DMA off)

        ; CTL first (disarms), DATA last (arms).
        move.w #$0090,$140(a6)    ; SPR0POS: VSTART 0
        move.w #$0000,$142(a6)    ; SPR0CTL: VSTOP 0
        move.w #$0000,$146(a6)    ; SPR0DATB
        move.w #$ffff,$144(a6)    ; SPR0DATA (arms)
        move.w #$50a8,$150(a6)    ; SPR2POS: VSTART $50
        move.w #$6000,$152(a6)    ; SPR2CTL: VSTOP $60
        move.w #$0000,$156(a6)
        move.w #$ffff,$154(a6)
        move.w #$c0c0,$160(a6)    ; SPR4POS: VSTART $C0
        move.w #$4000,$162(a6)    ; SPR4CTL: VSTOP $40
        move.w #$0000,$166(a6)
        move.w #$ffff,$164(a6)
.halt:  bra.s .halt
