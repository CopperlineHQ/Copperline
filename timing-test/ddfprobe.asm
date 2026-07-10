; DDF / bitplane-mode horizontal-placement probe.
;
; Loaded by boot.asm to $30000, takes over the machine (no OS, DMA/interrupts
; off) and puts up a STATIC display split into horizontal bands, each in a
; different bitplane mode and DDF window. Plane 1 is a "ruler" ($8000 every
; word = one white pixel every 16 lo-res px); planes 2-6 are zero. So in every
; band the left-most white line is the content's left edge (the fetch origin)
; and the right-most is the fetched-width end -- measurable to the pixel with no
; rotozoom/HAM-data/sprite confounds.
;
; Run on Copperline and vAmiga (KS 2.05, A500 OCS PAL 512+512) and compare the
; left/right white-line x of each band. Any difference is a placement-model bug.
;
; Bands (COLOR00 tags them; DDFSTOP shown, DIW wide-open so nothing is clipped):
;   0  vpos $30 BPLCON0 $1200  1 plane        DDFSTRT $38 DDFSTOP $D0  (reference)
;   1  vpos $50 BPLCON0 $6200  6 planes       DDFSTRT $38 DDFSTOP $D0
;   2  vpos $70 BPLCON0 $7200  BPU=7 overprog DDFSTRT $38 DDFSTOP $D0  (genx2 mosaic)
;   3  vpos $90 BPLCON0 $6A00  HAM6           DDFSTRT $38 DDFSTOP $D0
;   4  vpos $B0 BPLCON0 $7A00  HAM+BPU7       DDFSTRT $50 DDFSTOP $A8  (2nd-nature)
;   5  vpos $D0 BPLCON0 $6A00  HAM6           DDFSTRT $50 DDFSTOP $A8

CUST    equ     $dff000
RULER   equ     $40000          ; plane 1: $8000-per-word ruler
ZERO    equ     $50000          ; planes 2-6: zero
FILLW   equ     14336

;----------------------------------------------------------------- entry
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)  ; INTENA off
        move.w  #$7fff,$9c(a6)  ; INTREQ clear
        move.w  #$7fff,$96(a6)  ; DMACON off

        lea     RULER,a0
        move.w  #FILLW-1,d0
.fillr:
        move.w  #$8000,(a0)+
        dbra    d0,.fillr

        lea     ZERO,a0
        move.w  #FILLW-1,d0
.fillz:
        clr.w   (a0)+
        dbra    d0,.fillz

        lea     copperlist(pc),a0
        move.l  a0,$80(a6)      ; COP1LC
        move.w  d0,$88(a6)      ; COPJMP1 strobe

        move.w  #$8380,$96(a6)  ; DMAEN | BPLEN | COPEN
.loop:
        bra.s   .loop

;----------------------------------------------------------------- copper list
        cnop    0,4
copperlist:
        dc.w    $0102,$0000     ; BPLCON1
        dc.w    $0108,$0000     ; BPL1MOD (odd planes)
        dc.w    $010a,$0000     ; BPL2MOD (even planes)
        dc.w    $0182,$0fff     ; COLOR01 = white
        dc.w    $008e,$2c50     ; DIWSTRT wide-open left  (H=$50)
        dc.w    $0090,$2cd0     ; DIWSTOP wide-open right  (H=$D0)
        ; six bitplane pointers: plane 1 = ruler, planes 2-6 = zero
        dc.w    $00e0,$0004     ; BPL1PTH  ($40000)
        dc.w    $00e2,$0000     ; BPL1PTL
        dc.w    $00e4,$0005     ; BPL2PTH  ($50000)
        dc.w    $00e6,$0000
        dc.w    $00e8,$0005     ; BPL3PTH
        dc.w    $00ea,$0000
        dc.w    $00ec,$0005     ; BPL4PTH
        dc.w    $00ee,$0000
        dc.w    $00f0,$0005     ; BPL5PTH
        dc.w    $00f2,$0000
        dc.w    $00f4,$0005     ; BPL6PTH
        dc.w    $00f6,$0000

        ; band 0: 1 plane, DDFSTRT $38 / DDFSTOP $D0 (reference)
        dc.w    $0180,$0002
        dc.w    $0092,$0038
        dc.w    $0094,$00d0
        dc.w    $0100,$1200
        dc.w    $5001,$ff00
        ; band 1: 6 planes, DDFSTRT $38 / DDFSTOP $D0
        dc.w    $0180,$0202
        dc.w    $0100,$6200
        dc.w    $7001,$ff00
        ; band 2: BPU=7 overprogrammed (genx2 mosaic), DDFSTRT $38 / DDFSTOP $D0
        dc.w    $0180,$0020
        dc.w    $0100,$7200
        dc.w    $9001,$ff00
        ; band 3: HAM6, DDFSTRT $38 / DDFSTOP $D0
        dc.w    $0180,$0220
        dc.w    $0100,$6a00
        dc.w    $b001,$ff00
        ; band 4: HAM + BPU=7 (second-nature TV), DDFSTRT $50 / DDFSTOP $A8
        dc.w    $0180,$0002
        dc.w    $0092,$0050
        dc.w    $0094,$00a8
        dc.w    $0100,$7a00
        dc.w    $d001,$ff00
        ; band 5: HAM6, DDFSTRT $50 / DDFSTOP $A8
        dc.w    $0180,$0202
        dc.w    $0100,$6a00
        dc.w    $ffff,$fffe     ; end
