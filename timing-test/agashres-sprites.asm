; AGA SPRES sprite-resolution probe.
;
; Four otherwise identical 16-bit solid sprites are shown over a lo-res
; playfield while BPLCON3 SPRES steps through all four encodings:
;
;   SPRES 00  follow the lo-res playfield    32 framebuffer px
;   SPRES 01  forced lo-res                  32 framebuffer px
;   SPRES 10  forced hi-res                  16 framebuffer px
;   SPRES 11  forced super-hi-res             8 framebuffer px
;
; The resulting 4:4:2:1 staircase is the hardware distinction under test.
; In particular, SPRES 11 emits 35 ns samples; treating it like SPRES 10
; makes the bottom two bars the same width. Cross-checked against vAmigaTS
; Denise/Sprites/aga/simple2 and its included A1200 photograph.
CUST    equ $dff000
BMP     equ $40000
DESC0   equ $48000
DESC1   equ $48100
DESC2   equ $48200
DESC3   equ $48300
TERM    equ $48400
CLIST   equ $60000

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        ; Seed the AGA palette selection and both sprite-pair colours before
        ; the Copper is enabled. The Copper repeats these writes below, but
        ; the CPU setup also makes the first active line independent of where
        ; the bootstrap happened to release us within the current frame.
        move.w #$0000,$106(a6)   ; BPLCON3: SPRES 00
        move.w #$0011,$10c(a6)   ; BPLCON4: classic sprite palette bank
        move.w #$0ff0,$1a2(a6)   ; COLOR17: sprite pair 0/1
        move.w #$0ff0,$1aa(a6)   ; COLOR21: sprite pair 2/3

        ; A zero plane keeps the whole display window at COLOR00 while its
        ; DMA supplies the BPL1DAT edge that gates ordinary sprite output.
        lea BMP,a0
        move.w #$3fff,d0
.fill:  clr.w (a0)+
        dbra d0,.fill

        lea DESC0,a0
        move.w #$31,d1            ; first sprite line
        bsr.w make_sprite
        lea DESC1,a0
        move.w #$49,d1
        bsr.w make_sprite
        lea DESC2,a0
        move.w #$61,d1
        bsr.w make_sprite
        lea DESC3,a0
        move.w #$79,d1
        bsr.w make_sprite
        lea TERM,a0
        clr.w (a0)+
        clr.w (a0)+

        lea CLIST,a1
        move.l #$01fc0000,(a1)+   ; FMODE: 16-bit sprite fetches
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: sprites in front
        move.l #$01060000,(a1)+   ; BPLCON3: SPRES 00
        move.l #$010c0011,(a1)+   ; BPLCON4: classic sprite palette bank
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800024,(a1)+   ; COLOR00: dark blue
        move.l #$01a20ff0,(a1)+   ; COLOR17: yellow
        move.l #$01aa0ff0,(a1)+   ; COLOR21: yellow (sprite pair 2/3)
        move.l #$00e00004,(a1)+   ; BPL1PT = BMP
        move.l #$00e20000,(a1)+

        ; SPR0..3 point at the four bars; unused channels sit on the null
        ; descriptor so their line-25 control fetch cannot arm garbage.
        move.l #$01200004,(a1)+
        move.l #$01228000,(a1)+
        move.l #$01240004,(a1)+
        move.l #$01268100,(a1)+
        move.l #$01280004,(a1)+
        move.l #$012a8200,(a1)+
        move.l #$012c0004,(a1)+
        move.l #$012e8300,(a1)+
        move.w #$0130,d2
        moveq #4-1,d3
.null: move.w d2,(a1)+
        move.w #$0004,(a1)+
        addq.w #2,d2
        move.w d2,(a1)+
        move.w #$8400,(a1)+
        addq.w #2,d2
        dbra d3,.null

        ; Change only SPRES at the top of each 24-line section.
        move.l #$3001fffe,(a1)+
        move.l #$01060000,(a1)+
        move.l #$4801fffe,(a1)+
        move.l #$01060040,(a1)+
        move.l #$6001fffe,(a1)+
        move.l #$01060080,(a1)+
        move.l #$7801fffe,(a1)+
        move.l #$010600c0,(a1)+
        move.l #$9001fffe,(a1)+
        move.l #$01000200,(a1)+   ; display off below the probe
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$080(a6)
        move.w d0,$088(a6)
        move.w #$83a0,$096(a6)   ; DMAEN|BPLEN|COPEN|SPREN
.halt: bra.s .halt

; a0 = descriptor, d1 = VSTART. Each bar is 16 lines of colour 1 at the
; same HSTART ($A0 in lo-res coordinates -> POS low byte $50).
make_sprite:
        move.w d1,d2
        lsl.w #8,d2
        or.w #$0050,d2
        move.w d2,(a0)+           ; POS
        add.w #$10,d1
        lsl.w #8,d1
        move.w d1,(a0)+           ; CTL / VSTOP
        moveq #16-1,d2
.line: move.w #$ffff,(a0)+        ; DATA: solid colour 1
        clr.w (a0)+               ; DATB
        dbra d2,.line
        clr.w (a0)+               ; terminating POS
        clr.w (a0)+               ; terminating CTL
        rts
