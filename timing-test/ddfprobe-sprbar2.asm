; Manual vs DMA sprite position probe (same POS words).
;
; SPR6 (red, index 3):  manual bar, POS=$284F CTL=$3003 DATA/DATB=$F000
;                       written by the copper each frame at v=$F0; sprite
;                       DMA never touches channel 6 (its pointer parks on a
;                       terminator).
; SPR0 (blue, index 3): DMA sprite, descriptor POS=$604F CTL=$6801 (same
;                       hstart bits as the manual bar: H8-1=$4F<<1, H0=1),
;                       8 data lines of $F000/$F000, terminated. SPR0PT is
;                       rewritten by the copper at the list top each frame.
;
; Both bars share the identical horizontal position words, drawn over a
; 16-px ruler bitmap. Any x offset between the red and blue bars is a
; manual-vs-DMA placement difference; compare against vAmiga/FS-UAE/real.
CUST   equ $dff000
RULER  equ $40000
DESC   equ $48000
DESC6  equ $49000
CLIST  equ $60000
FILLW  equ 14336

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea RULER,a0
        move.w #FILLW-1,d0
.fr:    move.w #$8000,(a0)+      ; 1 white px every 16 lo-res px
        dbra d0,.fr

        ; sprite 0 DMA descriptor: control, 8 data lines, terminator
        lea DESC,a0
        move.w #$604f,(a0)+       ; POS: vstart=$60, H8-1=$4F<<1
        move.w #$6801,(a0)+       ; CTL: vstop=$68, H0=1
        moveq  #7,d0
.fd:    move.w #$f000,(a0)+       ; DATA
        move.w #$f000,(a0)+       ; DATB
        dbra   d0,.fd
        clr.w  (a0)+              ; terminator
        clr.w  (a0)+
        ; sprite 6 parked descriptor: immediate terminator
        lea DESC6,a0
        clr.w (a0)+
        clr.w (a0)+

        ; ---- build copper list at $60000 ----
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, colour on
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT $38
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01800113,(a1)+   ; COLOR00 dark blue background
        move.l #$01820fff,(a1)+   ; COLOR01 white ruler
        move.l #$0198004f,(a1)+   ; COLOR19 blue (SPR0/1 pair index 3)
        move.l #$01be0f00,(a1)+   ; COLOR31 red (SPR6/7 pair index 3)
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$01200004,(a1)+   ; SPR0PTH = $48000 (re-seed every frame)
        move.l #$01228000,(a1)+
        move.l #$01240004,(a1)+   ; SPR1PT -> terminator
        move.l #$01269000,(a1)+
        move.l #$01280004,(a1)+   ; SPR2PT -> terminator
        move.l #$012a9000,(a1)+
        move.l #$012c0004,(a1)+   ; SPR3PT
        move.l #$012e9000,(a1)+
        move.l #$01300004,(a1)+   ; SPR4PT
        move.l #$01329000,(a1)+
        move.l #$01340004,(a1)+   ; SPR5PT
        move.l #$01369000,(a1)+
        move.l #$01380004,(a1)+   ; SPR6PT
        move.l #$013a9000,(a1)+
        move.l #$013c0004,(a1)+   ; SPR7PT
        move.l #$013e9000,(a1)+

        move.l #$f001ff00,(a1)+   ; WAIT v=$F0
        move.l #$0170284f,(a1)+   ; SPR6POS (manual bar, same hstart bits)
        move.l #$01723003,(a1)+   ; SPR6CTL (disarms)
        move.l #$0174f000,(a1)+   ; SPR6DATA (re-arms)
        move.l #$0176f000,(a1)+   ; SPR6DATB
        move.l #$fffffffe,(a1)+   ; end

        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
.l:     bra.s .l
