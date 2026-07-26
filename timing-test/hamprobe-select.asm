; BPLCON0 HAM-select placement probe.
;
; The HAM bit does not feed Denise's bitplane shifter: it selects how the
; already-serialized index is turned into a colour, the same final stage a
; COLORxx write feeds. A mid-line HAM select therefore takes effect at the
; pixel a COLORxx write carried by the same chip-bus slot would change, not
; at the later position the generic register/beam domain places bitplane
; control writes at.
;
; Regression example: Hollywood Poker Pro paints a HAM photo on the left of
; the screen and an ordinary 6-bitplane scoreboard on the right of the SAME
; scanlines, switching HAM off with a Copper MOVE behind WAIT hp=$A2. Placed
; in the generic register domain the switch landed 26 lo-res pixels late, so
; the scoreboard's left 24 columns were decoded as HAM modify-blue: the panel
; greys came out as pure blues of the same intensity.
;
; The screen is 6-bitplane lo-res with every pixel index $1F (planes 1-5 set,
; plane 6 clear), except one marker column every 16 lo-res pixels where plane
; 1 is clear (index $1E):
;
;   HAM on  : $1F is control code 01 (modify blue) with value $F, so every
;             pixel is (R,G) held from the black background and B=$F -- a
;             solid blue field. $1E differs only in blue intensity, so the
;             markers are invisible here by design.
;   HAM off : six planes without HAM is the plain (EHB-capable) index path
;             and bit 5 is clear, so $1F is COLOR31 (green) and $1E is
;             COLOR30 (white) -- a green field with white marker columns.
;
; The blue/green boundary is the HAM select's landing column, and the marker
; columns are the ruler to read it against.
;
; Bands (PAL, DIWSTRT $2C81 / DIWSTOP $2CC1, 256 display lines from v=$2C):
;
;   v $2C..$3B   HAM off for the whole line -- full-width green reference
;                row carrying every ruler mark.
;   v $3C..$BB   eight 16-line bands. Each line restores HAM at hp=$07 (deep
;                in the horizontal blank, so the whole visible line starts in
;                HAM) and clears it again at hp = $40 + $10 * band, giving a
;                staircase of blue/green boundaries 32 lo-res pixels apart.
;   v $BC..$12B  HAM left on -- solid blue reference block.
;
; Expected settled render: a green ruler band on top, then a descending
; blue-left/green-right staircase whose steps march right by two ruler marks
; per band, then a solid blue block. Each boundary sits at the lo-res pixel
; the same-slot COLORxx write would reach.
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh).
CUST   equ $dff000
PLANE1 equ $40000               ; plane 1: solid with ruler bits cleared
SOLID  equ $44000               ; planes 2-5 (shared): all ones
ZEROS  equ $48000               ; plane 6: all zeros
CLIST  equ $60000
ROWB   equ 40                   ; bytes per bitplane row (320 lo-res px)
ROWS   equ 256                  ; display lines
PLSIZE equ ROWB*ROWS

        lea CUST,a6
        move.w #$7fff,$9a(a6)     ; INTENA: all off
        move.w #$7fff,$9c(a6)     ; INTREQ: clear
        move.w #$7fff,$96(a6)     ; DMACON: all off

        ; ---- bitplane buffers ----
        lea SOLID,a0
        move.w #PLSIZE/2-1,d0
.fs:    move.w #$ffff,(a0)+
        dbra d0,.fs

        lea ZEROS,a0
        move.w #PLSIZE/2-1,d0
.fz:    clr.w (a0)+
        dbra d0,.fz

        lea PLANE1,a0
        move.w #PLSIZE/2-1,d0
.fr:    move.w #$7fff,(a0)+       ; clear px 0 of every 16 -> ruler mark
        dbra d0,.fr

        ; ---- copper list preamble ----
        lea CLIST,a1
        move.l #$01006200,(a1)+   ; BPLCON0: 6 planes, lo-res, HAM off
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$01800000,(a1)+   ; COLOR00 black (the HAM carry seed)
        move.l #$01bc0fff,(a1)+   ; COLOR30 white (ruler, HAM off)
        move.l #$01be00f0,(a1)+   ; COLOR31 green (field, HAM off)
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE1
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = SOLID
        move.l #$00e64000,(a1)+
        move.l #$00e80004,(a1)+   ; BPL3PT = SOLID
        move.l #$00ea4000,(a1)+
        move.l #$00ec0004,(a1)+   ; BPL4PT = SOLID
        move.l #$00ee4000,(a1)+
        move.l #$00f00004,(a1)+   ; BPL5PT = SOLID
        move.l #$00f24000,(a1)+
        move.l #$00f40004,(a1)+   ; BPL6PT = ZEROS
        move.l #$00f68000,(a1)+

        ; ---- eight 16-line bands, HAM cleared 16 colour clocks later each --
        move.w #$003c,d3          ; first band line
        move.w #$0041,d4          ; WAIT hp field for colour clock $40
        moveq #8-1,d2
.band:  moveq #16-1,d5
.line:  move.w d3,d0
        lsl.w #8,d0
        move.w d0,d1
        or.w #$0007,d1            ; WAIT vp=d3 hp=$07 (horizontal blank)
        move.w d1,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01006a00,(a1)+   ; BPLCON0: HAM on
        or.w d4,d0                ; WAIT vp=d3 hp=band colour clock
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01006200,(a1)+   ; BPLCON0: HAM off
        addq.w #1,d3
        dbra d5,.line
        add.w #$0010,d4
        dbra d2,.band

        ; ---- HAM back on for the bottom reference block ----
        move.w d3,d0
        lsl.w #8,d0
        or.w #$0007,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01006a00,(a1)+

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
