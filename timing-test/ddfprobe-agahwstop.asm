; AGA hardwired DDF stop probe (lo-res, FMODE=0).
;
; Alice keeps the ECS hardwired $D8 bitplane-fetch stop: a DDFSTOP past it
; is cut short there unless BEAMCON0 selects HARDDIS or VARBEAMEN, or
; BPLCON0 selects SHRES or UHRES.  DDFSTRT $28 / DDFSTOP $E0 therefore
; fetches 23 words per line ($28..$D0 plus the $D8 drain unit), not the 24
; the raw DDFSTOP would give.
;
; The bitmap repeats one 23-word row, and BPL1MOD = -46 rewinds the pointer
; to the same row when exactly 23 words were fetched, so the marker blocks
; stand as straight vertical bars.  A 24-word fetch advances the pointer two
; bytes per line and shears every marker into a diagonal stripe (the
; Pacmania WHDLoad AGA regression class).
;
; Bands:
;   top    ($2C-$9F)  BEAMCON0 = PAL: hard stop in force -> straight bars.
;   middle ($A0-$FF)  BEAMCON0 = PAL|HARDDIS: the stop relaxes to DDFSTOP,
;                     24 words per line -> markers shear one word per line.
;   bottom ($100-end) BEAMCON0 = PAL again -> straight bars, shifted by the
;                     middle band's accumulated drift.
;
; Cross-check basis: WinUAE `check_harddis` (harddis_h = ECS Agnus and
; VARBEAMEN|HARDDIS|SHRES|UHRES; the AGA chipset mask includes ECS Agnus)
; and vAmiga 5's sequencer, which takes the ECS path on AGA and raises RHW
; at $D8 for every revision.  vAmiga does not model HARDDIS, so its render
; of the middle band shows straight bars; only the top band is a valid
; vAmiga reference.  The Pacmania WHDLoad install renders correctly on an
; FS-UAE (WinUAE core) A1200 (2026-10-14).
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        lea BMP,a0
        moveq #32-1,d1            ; 32 copies of the 23-word row
.row:   lea rowdata(pc),a1
        moveq #23-1,d0
.word:  move.w (a1)+,(a0)+
        dbra d0,.word
        dbra d1,.row

        lea CLIST,a1
        move.l #$01dc0020,(a1)+   ; BEAMCON0 = PAL (hard stop in force)
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane, lo-res
        move.l #$01020000,(a1)+   ; BPLCON1: no scroll
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$01060c00,(a1)+   ; BPLCON3
        move.l #$01fc0000,(a1)+   ; FMODE = 0
        move.l #$0108ffd2,(a1)+   ; BPL1MOD = -46 bytes (23-word rewind)
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920028,(a1)+   ; DDFSTRT $28
        move.l #$009400e0,(a1)+   ; DDFSTOP $E0 (past the $D8 hard stop)
        move.l #$008e2c81,(a1)+   ; standard DIWSTRT
        move.l #$00902cc1,(a1)+   ; standard DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL
        move.l #$a001fffe,(a1)+   ; WAIT line $A0
        move.l #$01dc4020,(a1)+   ; BEAMCON0 = PAL|HARDDIS
        move.l #$ffdffffe,(a1)+   ; WAIT past line $FF
        move.l #$0001fffe,(a1)+   ; WAIT line $100
        move.l #$01dc0020,(a1)+   ; BEAMCON0 = PAL
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.loop:  bra.s .loop

; Words 0-1 and 22 sit outside the standard window ($28 fetches two words
; left of DIWSTRT $81).  Solid blocks at irregular word positions make a
; one-word-per-line shear unmistakable.
rowdata:
        dc.w $0000,$0000,$ffff,$0000,$0000,$ff00,$0000,$0000
        dc.w $0000,$ffff,$ffff,$0000,$0000,$0000,$0000,$00ff
        dc.w $0000,$0000,$ffff,$0000,$0000,$ffff,$0000
