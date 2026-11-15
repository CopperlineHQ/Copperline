; CPU chip-window image probe (border-colour stripes).
;
; The motherboard address decode routes every CPU access in
; $000000-$1FFFFF to Agnus, and Agnus decodes only the address bits it is
; wired to see. On a machine with 512K of chip RAM the first DRAM bank
; therefore answers again at $080000, $100000 and $180000:
;
;   - OCS 8370/8371: A19 and A20 are not decoded at all.
;   - ECS 8372A: A20 is not decoded, and the board straps the chip's A19
;     input (JP2 on the A500 rev 6A) to CPU A23 while the second DRAM bank
;     is trapdoor RAM at $C00000, so chip-window accesses arrive with A19
;     low.
;
; Loaders size chip RAM with exactly this test (write above 512K, look for
; the write at the bottom), so a missing image sends them into RAM that is
; not there.
;
; With display DMA off the whole raster shows COLOR00. For each of the
; three images the probe makes two checks against a cell inside its own
; code, then paints one stripe per check, top to bottom:
;
;   lines $40..$4F: white reference stripe
;   lines $50..$67: $080000 image, write through it   (green = landed in
;                   the first bank, red = it did not)
;   lines $68..$7F: $080000 image, read through it    (green = read the
;                   first bank's cell back, red = something else)
;   lines $80..$AF: the same pair for the $100000 image
;   lines $B0..$DF: the same pair for the $180000 image
;   line  $E0:      back to the dark background
;
; Expected on a 512K-chip machine, OCS or 8372A: six green stripes. A 1M
; chip machine has real RAM at $080000 and shows the first pair red.
;
; vAmiga 4.4 (A500_OCS_1MB and A500_ECS_1MB) renders six green stripes.
CUST   equ $dff000
IMAGE  equ $80000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        move.w #$0113,$180(a6)    ; background

        lea    cell(pc),a0
        lea    stripes(pc),a1
        move.l #IMAGE,d1
        moveq  #2,d7              ; three images
.img:   move.l a0,a2
        add.l  d1,a2              ; the cell's address inside this image
        move.w #$1111,(a0)
        move.w #$a55a,(a2)        ; write through the image
        move.w #$0f00,d0
        cmp.w  #$a55a,(a0)
        bne.s  .wr
        move.w #$00f0,d0
.wr:    move.w d0,(a1)+
        move.w #$3c3c,(a0)
        move.w #$0f00,d0
        cmp.w  #$3c3c,(a2)        ; read through the image
        bne.s  .rd
        move.w #$00f0,d0
.rd:    move.w d0,(a1)+
        add.l  #IMAGE,d1
        dbra   d7,.img

frame:
        ; frame sync: wait for V8 to rise (line >= 256), then fall (wrap).
.f1:    move.l $04(a6),d0
        btst   #16,d0
        beq.s  .f1
.f2:    move.l $04(a6),d0
        btst   #16,d0
        bne.s  .f2

        move.w #$40,d2
        bsr.s  lwait
        move.w #$0fff,$180(a6)    ; reference stripe
        lea    stripes(pc),a1
        move.w #$50,d2
        moveq  #5,d7
.bar:   bsr.s  lwait
        move.w (a1)+,$180(a6)
        add.w  #$18,d2
        dbra   d7,.bar
        bsr.s  lwait              ; d2 = $E0
        move.w #$0113,$180(a6)    ; background below the stripes
        bra.s  frame

; Spin until VHPOSR's V7-0 reads d2.
lwait:  move.w $06(a6),d0
        lsr.w  #8,d0
        cmp.b  d2,d0
        bne.s  lwait
        rts

cell:    dc.w 0
stripes: dcb.w 6,0
