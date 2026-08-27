; AGA HAM8 decode probe.
;
; HAM8 inverts HAM6's bit assignment: the CONTROL bits are the two LOWEST
; planes (pixel bits 0-1) and planes 3-8 carry a six-bit value.
;   00  set from base palette entry value (0-63)
;   01  modify blue   10  modify red   11  modify green
; A modify replaces the top six bits of the 8-bit component and holds the
; low two bits from the previous pixel's component. Every non-set pixel
; therefore depends on the full pixel history of its line: resolving HAM8
; through a per-index cache renders the ramps as flat bars (the PR #563
; regression class), and putting the control bits on the two highest
; planes (HAM6's layout) scrambles every band.
;
; Display: 8-plane lo-res FMODE=0 HAM8, six bands of one repeated row
; each (every row starts with a set op, so no state crosses lines):
;   band 1  set-op sweep: entries 0-39 in 8-px steps -- pins the op-00
;           palette lookup and the BPLCON3 BANK write path (entries 32-39
;           live in bank 1)
;   band 2  set black, then modify-red values ascending in 1-px steps: a
;           red ramp rising to near-full red, wrapping once
;   band 3  the same ramp through modify-green
;   band 4  the same ramp through modify-blue
;   band 5  set entry 5, then a weave of modify ops cycling B/R/G with a
;           stepping value sequence: every pixel depends on the chain
;           before it (the indexed-cache tell)
;   band 6  the low-bit hold: the left half seeds red=$83 (low bits 11,
;           written via BPLCON3 LOCT), the right half seeds red=$80, and
;           both run the same modify-red ramp -- the halves must differ by
;           exactly those held low bits all the way across
;
; Cross-checked against vAmiga 5 (tools/vamiga-ref.sh, A1200_2MB) under its
; identity RGB monitor palette (VAMIGA_RGB=1): byte-exact over the whole
; frame, low-bit hold included.
CUST    equ $dff000
PLANES  equ $40000                ; 8 plane buffers, $3000 apart
PSTRIDE equ $3000
CHUNK   equ $58000                ; 320-byte chunky row scratch
PROW    equ $58200                ; 8 x 40-byte planar row scratch
CLIST   equ $60000
ROWS    equ 256                   ; DIW v44..299
BANDH   equ 42                    ; band height (the last band takes the
                                  ; remaining ROWS-5*BANDH = 46 rows)

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        ; ---- build the six bands ----
        moveq #0,d6               ; first row of the current band
        lea band1(pc),a4
        move.w #BANDH,d7
        bsr bldband
        lea band2(pc),a4
        move.w #BANDH,d7
        bsr bldband
        lea band3(pc),a4
        move.w #BANDH,d7
        bsr bldband
        lea band4(pc),a4
        move.w #BANDH,d7
        bsr bldband
        lea band5(pc),a4
        move.w #BANDH,d7
        bsr bldband
        lea band6(pc),a4
        move.w #ROWS-5*BANDH,d7
        bsr bldband
        bra clist

; ---- band chunky-row generators: fill CHUNK with 320 index bytes ----
band1:  lea CHUNK,a0              ; set ops, entry = px/8 (0..39)
        moveq #0,d0               ; running entry<<2
        moveq #40-1,d1
.b1:    moveq #8-1,d2
.b1p:   move.b d0,(a0)+
        dbra d2,.b1p
        addq.b #4,d0
        dbra d1,.b1
        rts

band2:  moveq #2,d3               ; modify-red
        bra.s ramp
band3:  moveq #3,d3               ; modify-green
        bra.s ramp
band4:  moveq #1,d3               ; modify-blue
ramp:   lea CHUNK,a0
        clr.b (a0)+               ; set entry 0 (black)
        moveq #0,d0               ; value byte, wraps naturally
        move.w #319-1,d1
.rp:    move.b d0,d2
        and.b #$fc,d2
        or.b d3,d2
        move.b d2,(a0)+
        addq.b #1,d0
        dbra d1,.rp
        rts

band5:  lea CHUNK,a0              ; history weave
        move.b #$14,(a0)+         ; set entry 5
        moveq #0,d0               ; value accumulator (+7 per px)
        moveq #1,d3               ; op cycles 1,2,3
        move.w #319-1,d1
.wv:    move.b d0,d2
        and.b #$fc,d2
        or.b d3,d2
        move.b d2,(a0)+
        addq.b #7,d0
        addq.b #1,d3
        cmp.b #4,d3
        bne.s .wk
        moveq #1,d3
.wk:    dbra d1,.wv
        rts

band6:  lea CHUNK,a0              ; low-bit hold halves
        move.b #$18,(a0)+         ; set entry 6 (red=$83)
        moveq #0,d0
        move.w #159-1,d1
.h1:    move.b d0,d2
        and.b #$fc,d2
        or.b #2,d2                ; modify-red
        move.b d2,(a0)+
        addq.b #1,d0
        dbra d1,.h1
        move.b #$1c,(a0)+         ; set entry 7 (red=$80)
        moveq #0,d0
        move.w #159-1,d1
.h2:    move.b d0,d2
        and.b #$fc,d2
        or.b #2,d2
        move.b d2,(a0)+
        addq.b #1,d0
        dbra d1,.h2
        rts

; ---- bldband: generator a4, band rows d7, band origin row d6 ----
bldband:
        jsr (a4)
        ; c2p: CHUNK (320 bytes) -> PROW (8 planes x 40 bytes)
        moveq #0,d5               ; plane
.plane: lea CHUNK,a0
        lea PROW,a1
        move.w d5,d0
        mulu #40,d0
        adda.w d0,a1              ; this plane's 40-byte row
        moveq #20-1,d1            ; words per row
.word:  moveq #0,d2               ; assembled word
        moveq #16-1,d3
.bit:   add.w d2,d2
        move.b (a0)+,d4
        lsr.b d5,d4
        and.w #1,d4
        or.w d4,d2
        dbra d3,.bit
        move.w d2,(a1)+
        dbra d1,.word
        addq.w #1,d5
        cmp.w #8,d5
        bne.s .plane
        ; replicate PROW into each plane buffer for this band's rows
        moveq #0,d5               ; plane
.rplane:
        lea PROW,a0
        move.w d5,d0
        mulu #40,d0
        adda.w d0,a0              ; this plane's source row
        lea PLANES,a1
        move.w d5,d0
        mulu #PSTRIDE,d0
        adda.l d0,a1              ; plane buffer base
        move.w d6,d0
        mulu #40,d0
        adda.l d0,a1              ; first row of the band
        move.w d7,d1
        subq.w #1,d1
.rrow:  move.l (a0),(a1)+         ; copy 40 bytes (10 longs)
        move.l 4(a0),(a1)+
        move.l 8(a0),(a1)+
        move.l 12(a0),(a1)+
        move.l 16(a0),(a1)+
        move.l 20(a0),(a1)+
        move.l 24(a0),(a1)+
        move.l 28(a0),(a1)+
        move.l 32(a0),(a1)+
        move.l 36(a0),(a1)+
        dbra d1,.rrow
        addq.w #1,d5
        cmp.w #8,d5
        bne.s .rplane
        add.w d7,d6               ; advance the band origin
        rts

; ---- copper list ----
clist:  lea CLIST,a1
        move.l #$01000a10,(a1)+   ; BPLCON0: 8 planes (BPU3), HAM
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$010c0011,(a1)+   ; BPLCON4
        move.l #$01fc0000,(a1)+   ; FMODE = 0
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        ; bank 0 palette: entries 0-31, value = i * $0137 (i=0 -> black)
        move.l #$01060000,(a1)+   ; BPLCON3: bank 0
        moveq #0,d0               ; entry
        moveq #0,d1               ; value
.pal0:  move.w #$0180,d2
        add.w d0,d2
        add.w d0,d2
        move.w d2,(a1)+
        move.w d1,d2
        and.w #$0fff,d2
        move.w d2,(a1)+
        add.w #$0137,d1
        addq.w #1,d0
        cmp.w #32,d0
        bne.s .pal0
        ; entries 6/7: grey with red high nibble 8 (the non-LOCT write
        ; mirrors the low nibbles), then the LOCT pass sets the true low
        ; nibbles: red low bits 3 for entry 6, 0 for entry 7
        move.l #$018c0888,(a1)+   ; COLOR06 high nibbles
        move.l #$018e0888,(a1)+   ; COLOR07 high nibbles
        move.l #$01060200,(a1)+   ; BPLCON3: bank 0, LOCT
        move.l #$018c0388,(a1)+   ; COLOR06 low nibbles (red lo=3)
        move.l #$018e0088,(a1)+   ; COLOR07 low nibbles (red lo=0)
        ; bank 1: entries 32-39 continue the same value sequence
        move.l #$01062000,(a1)+   ; BPLCON3: bank 1
        moveq #0,d0
.pal1:  move.w #$0180,d2
        add.w d0,d2
        add.w d0,d2
        move.w d2,(a1)+
        move.w d1,d2
        and.w #$0fff,d2
        move.w d2,(a1)+
        add.w #$0137,d1
        addq.w #1,d0
        cmp.w #8,d0
        bne.s .pal1
        move.l #$01060000,(a1)+   ; BPLCON3 back to bank 0
        ; bitplane pointers: plane p at PLANES + p*$3000
        moveq #0,d0               ; plane
        move.l #PLANES,d1
.bpl:   move.w #$00e0,d2
        move.w d0,d3
        lsl.w #2,d3
        add.w d3,d2               ; BPLxPTH register
        move.w d2,(a1)+
        swap d1
        move.w d1,(a1)+
        swap d1
        addq.w #2,d2              ; BPLxPTL
        move.w d2,(a1)+
        move.w d1,(a1)+
        add.l #PSTRIDE,d1
        addq.w #1,d0
        cmp.w #8,d0
        bne.s .bpl
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
