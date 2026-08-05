; HAM hold-colour accumulation across the DIW left edge.
;
; Denise's HAM accumulator advances on every serialized sample; the display
; window only selects whether the border colour or the playfield output
; reaches the screen. With DDFSTRT one fetch period before the window
; (DDFSTRT $30, DIW HSTART $79 -- the classic 352px overscan pairing), the
; first eight lo-res samples of every line are fetched and shifted but
; border-masked, and the hold colour they establish is what the first
; visible pixel modifies.
;
; Regression example: the Lemmings 2 FES demo's DMA Design logo opens every
; line of its overscan HAM picture with a set-palette pixel in that hidden
; span. A renderer that bounds HAM history to the visible samples restarts
; each line from COLOR00 and decodes the left edge as bright modify-green
; streaks.
;
; The screen is 6-bitplane lo-res HAM. Every line's fetched samples are:
;
;   sample 0      (hidden) : set palette -- entry 1 in the top band,
;                            entry 2 in the bottom band
;   samples 1..7  (hidden) : modify blue := $0 (keeps the held R and G)
;   samples 8..   (visible): modify blue := $F on every sample
;
; so every visible pixel's R and G come from the hidden set-palette pixel
; and only blue is rewritten. With COLOR01 = $F00 and COLOR02 = $0F0:
;
;   correct   : top band solid magenta ($F0F), bottom band solid cyan ($0FF)
;   truncated : both bands identical blue ($00F) -- the hidden seed is lost
;               and each line restarts from COLOR00 black.
;
; The band split proves the visible field is driven by the hidden pixel
; alone: the two bands' visible fetch words are identical.
;
; Each plane displays one 22-word row repeated by a -44 modulo; the band
; boundary at v=$AC swaps BPL1PT/BPL2PT between the two row variants so
; sample 0 selects palette entry 2 instead of entry 1.
;
; Sprites stay disabled: the $30 fetch start overlaps the sprite 6/7 DMA
; slots, as in the Lemmings 2 screen (SPREN off).
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh): band colours and the
; playfield-open column agree. vAmiga is not a reference for the hidden span
; itself: it paints $71..$79 with the held playfield colour, while the real
; window edge stays border until DIW HSTART (the vAmigaTS Agnus/DIW/OLDDIW
; diw1 A500 photos place the edge flush with the standard-DDF picture).
CUST   equ $dff000
ROWX   equ $40000               ; $80FF,$FFFF*21: set-pal bit at sample 0
ROWY   equ $40100               ; $00FF,$FFFF*21: no sample-0 bit
ROWZ   equ $40200               ; $7FFF,$FFFF*21: modify-select plane 5
ROWW   equ $40300               ; zeros: plane 6 clear -> ctrl 01 (blue)
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)     ; INTENA: all off
        move.w #$7fff,$9c(a6)     ; INTREQ: clear
        move.w #$7fff,$96(a6)     ; DMACON: all off

        ; ---- one-row bitplane buffers (22 words each) ----
        lea ROWX,a0
        move.w #$80ff,(a0)+
        moveq #21-1,d0
.fx:    move.w #$ffff,(a0)+
        dbra d0,.fx

        lea ROWY,a0
        move.w #$00ff,(a0)+
        moveq #21-1,d0
.fy:    move.w #$ffff,(a0)+
        dbra d0,.fy

        lea ROWZ,a0
        move.w #$7fff,(a0)+
        moveq #21-1,d0
.fz:    move.w #$ffff,(a0)+
        dbra d0,.fz

        lea ROWW,a0
        moveq #22-1,d0
.fw:    clr.w (a0)+
        dbra d0,.fw

        ; ---- copper list ----
        lea CLIST,a1
        move.l #$01006a00,(a1)+   ; BPLCON0: 6 planes, lo-res, HAM
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$0108ffd4,(a1)+   ; BPL1MOD: -44, repeat the row
        move.l #$010affd4,(a1)+   ; BPL2MOD: -44
        move.l #$00920030,(a1)+   ; DDFSTRT: one fetch period early
        move.l #$009400d8,(a1)+   ; DDFSTOP
        move.l #$008e2c79,(a1)+   ; DIWSTRT: window 8 lo-res px early
        move.l #$00902cc9,(a1)+   ; DIWSTOP
        move.l #$01800000,(a1)+   ; COLOR00 black (the broken-case seed)
        move.l #$01820f00,(a1)+   ; COLOR01 red   (top-band hidden seed)
        move.l #$018400f0,(a1)+   ; COLOR02 green (bottom-band hidden seed)
        move.l #$00e00004,(a1)+   ; BPL1PT = ROWX (sample 0 -> entry 1)
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = ROWY
        move.l #$00e60100,(a1)+
        move.l #$00e80004,(a1)+   ; BPL3PT = ROWY
        move.l #$00ea0100,(a1)+
        move.l #$00ec0004,(a1)+   ; BPL4PT = ROWY
        move.l #$00ee0100,(a1)+
        move.l #$00f00004,(a1)+   ; BPL5PT = ROWZ
        move.l #$00f20200,(a1)+
        move.l #$00f40004,(a1)+   ; BPL6PT = ROWW
        move.l #$00f60300,(a1)+

        ; ---- bottom band: swap planes 1/2 so sample 0 selects entry 2 ----
        move.l #$ac07fffe,(a1)+   ; WAIT v=$AC hp=$07 (horizontal blank)
        move.l #$00e00004,(a1)+   ; BPL1PT = ROWY
        move.l #$00e20100,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = ROWX
        move.l #$00e60000,(a1)+

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)     ; COP1LC
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
