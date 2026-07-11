; CLXDAT collision matrix probe.
;
; Two overlapping bitplane rectangles plus a DMA sprite build a known
; collision scene; the CPU reads CLXDAT after two settled frames and
; renders its 16 bits as one-word cells (bit 0 leftmost), for two CLXCON
; programs:
;
;   rows 100..107: CLXCON $00C3 (planes 1-2 enabled, match 1). Cells: bit 0
;                  (odd-even playfield overlap), bit 1 (sprite 0 over the
;                  odd plane), bit 5 (sprite 0 over the even plane), and
;                  bit 15 (the unused CLXDAT bit reads high).
;   rows 112..119: CLXCON $01C7 (plane 3 ALSO enabled, beyond the 2-plane
;                  BPU). The missing plane reads 0 and never matches, which
;                  kills the odd-group MATCH condition: bit 1 drops out,
;                  while bit 0 persists (the dual-playfield odd-even overlap
;                  latches on pixel presence, not the CLXCON match) --
;                  pinning the enabled-plane-beyond-BPU semantics.
;
; The rectangles and sprite stay on screen, so any change to the live
; collision scan, the matching matrix, or sprite-playfield overlap
; placement moves the cells.
;
;   plane 1 (red):  rows  10..49, words  5..14
;   plane 2 (blue): rows  30..69, words 10..19
;   sprite 0:       v=$40..$50, x=$120 (words 9-10 of the window)
CUST   equ $dff000
BPL1   equ $40000
BPL2   equ $42800
DESC0  equ $48000
TERM   equ $48100
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        ; clear both planes
        lea BPL1,a0
        move.w #10240-1,d0
.cz:    clr.w (a0)+
        dbra d0,.cz
        ; plane 1 rectangle: rows 10..49, words 5..14
        lea BPL1+10*40+10,a0
        moveq #40-1,d1
.r1:    move.l a0,a2
        moveq #10-1,d0
.r1w:   move.w #$ffff,(a2)+
        dbra d0,.r1w
        lea 40(a0),a0
        dbra d1,.r1
        ; plane 2 rectangle: rows 30..69, words 10..19
        lea BPL2+30*40+20,a0
        moveq #40-1,d1
.r2:    move.l a0,a2
        moveq #10-1,d0
.r2w:   move.w #$ffff,(a2)+
        dbra d0,.r2w
        lea 40(a0),a0
        dbra d1,.r2
        ; sprite 0 descriptor: bar v=$40..$50 x=$120
        lea DESC0,a0
        move.w #$4090,(a0)+       ; POS
        move.w #$5000,(a0)+       ; CTL
        moveq #16-1,d0
.s0:    move.w #$ffff,(a0)+
        move.w #$0000,(a0)+
        dbra d0,.s0
        clr.w (a0)+
        clr.w (a0)+
        lea TERM,a0
        clr.w (a0)+
        clr.w (a0)+

        ; copper list
        lea CLIST,a1
        move.l #$01002600,(a1)+   ; BPLCON0: 2 planes, dual playfield
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040024,(a1)+   ; BPLCON2: PF2 priority + sprites above
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$0090acc1,(a1)+   ; DIWSTOP: close at line $AC (128 rows,
                                  ; the height of the two 128-row bitmaps)
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820f00,(a1)+   ; COLOR01 red (PF1 = plane 1)
        move.l #$0192000f,(a1)+   ; COLOR09 blue (PF2 = plane 2)
        move.l #$01a20ff0,(a1)+   ; COLOR17 yellow (sprite)
        move.l #$01a400f0,(a1)+   ; COLOR18
        move.l #$01a6008f,(a1)+   ; COLOR19
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$00e40004,(a1)+   ; BPL2PT = $42800
        move.l #$00e62800,(a1)+
        move.l #$01200004,(a1)+   ; SPR0PT = DESC0
        move.l #$01228000,(a1)+
        move.l #$01240004,(a1)+   ; SPR1..SPR7 -> TERM
        move.l #$01268100,(a1)+
        move.l #$01280004,(a1)+
        move.l #$012a8100,(a1)+
        move.l #$012c0004,(a1)+
        move.l #$012e8100,(a1)+
        move.l #$01300004,(a1)+
        move.l #$01328100,(a1)+
        move.l #$01340004,(a1)+
        move.l #$01368100,(a1)+
        move.l #$01380004,(a1)+
        move.l #$013a8100,(a1)+
        move.l #$013c0004,(a1)+
        move.l #$013e8100,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)

        ; pass 1: planes 1-2 enabled with match value 1
        move.w #$00c3,$98(a6)     ; CLXCON
        move.w $0e(a6),d0         ; clear any stale CLXDAT
        move.w #$83a0,$96(a6)     ; DMAEN|BPLEN|COPEN|SPREN
        bsr.s  settle
        move.w $0e(a6),d7         ; CLXDAT
        lea    BPL1+100*40+4,a3
        bsr.s  drawbits

        ; pass 2: plane 3 also enabled -- beyond the 2-plane BPU
        move.w #$01c7,$98(a6)
        move.w $0e(a6),d0         ; clear the latch for the new program
        bsr.s  settle
        move.w $0e(a6),d7
        lea    BPL1+112*40+4,a3
        bsr.s  drawbits
.halt:  bra.s  .halt

; Let two full frames render (V8 rise/fall twice).
settle: moveq #2-1,d2
.fs:    move.l $04(a6),d0
        btst   #16,d0
        beq.s  .fs
.fe:    move.l $04(a6),d0
        btst   #16,d0
        bne.s  .fe
        dbra   d2,.fs
        rts

; Render d7's 16 bits as adjacent one-word cells at a3 (bit 0 leftmost),
; 8 rows tall.
drawbits:
        moveq #16-1,d1
.cell:  btst   d1,d7
        beq.s  .skip
        move.w d1,d0
        add.w  d0,d0              ; bit index * 2 bytes
        lea    (a3),a0
        adda.w d0,a0
        moveq  #8-1,d0
.crow:  move.w #$ffff,(a0)
        lea    40(a0),a0
        dbra   d0,.crow
.skip:  dbra   d1,.cell
        rts
