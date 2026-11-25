; Area-fill timing does not depend on DESC (Copper-started BBUSY bars).
;
; The Copper starts each blit itself and then waits for BBUSY to drop
; (WAIT $0001,$7FFE: VP=HP=0 compare, blitter-finished bit clear), so the
; coloured run on each raster line is pure blitter time plus a constant
; Copper overhead. The CPU only builds the Copper list and then STOPs with
; interrupts masked, so it never takes a chip-bus cycle. Every blit is an
; A->D copy (BLTCON0 $09F0, USEA|USED, LF=A) of 10 rows x 2 words; only
; BLTCON1 changes from band to band:
;
;   band 1 (yellow, lines $40-$47): BLTCON1 $0000  ascending copy
;   band 2 (orange, lines $4C-$53): BLTCON1 $0002  descending copy
;   band 3 (green,  lines $58-$5F): BLTCON1 $0010  ascending EFE fill
;   band 4 (cyan,   lines $64-$6B): BLTCON1 $0008  ascending IFE fill
;   band 5 (pink,   lines $70-$77): BLTCON1 $0012  descending EFE fill
;
; Each bar ends in one white Copper move marking the instant BBUSY cleared.
; Hardware runs the fill micro-program (one idle cycle after each D cycle
; when USEC is clear: "A0 -- -- A1 D0 -- A2 D1 --") whenever IFE or EFE is
; set, whatever DESC says, so bands 3, 4 and 5 are the same length and one
; colour clock per word (20 CCK, 40 lo-res pixels) longer than bands 1 and
; 2, which match each other. Regression class: the fill idle cycle gated on
; DESC made the ascending fills (bands 3 and 4) as short as the copies.
; Hardware reference: vAmigaTS Agnus/Blitter/bbusy/copbbusy1f/5f/9f/13f
; (BLTCON1 $0010, the same Copper/BBUSY construction), photographed on an
; A500+ and an A1200, with the fill bar ending one ruler dash past the copy.
CUST   equ $dff000
SRC    equ $50000
DST    equ $52000
CLIST  equ $60000

LAST   equ 38                     ; byte offset of the last word (10x2 words)

        lea CUST,a6
        move.w #$7fff,$9a(a6)     ; INTENA off
        move.w #$7fff,$9c(a6)     ; INTREQ clear
        move.w #$7fff,$96(a6)     ; DMACON off
        lea SRC,a0                ; clear source and destination
        move.w #2048-1,d0
.cz:    clr.l (a0)+
        dbra d0,.cz
        lea clist(pc),a0
        lea CLIST,a1
        move.w #(clistend-clist)/2-1,d0
.cp:    move.w (a0)+,(a1)+
        dbra d0,.cp
        move.w #$0002,$2e(a6)     ; COPCON: CDANG, the Copper may write blitter regs
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$82c0,$96(a6)     ; SET DMAEN|COPEN|BLTEN (no bitplanes)
.halt:  stop #$2700
        bra.s .halt

; One measured line: \1 = line, \2 = BLTCON1, \3 = colour, \4 = APT/DPT offset.
BLITLINE MACRO
        dc.w $0040,$09f0          ; BLTCON0 USEA|USED LF=A
        dc.w $0042,\2             ; BLTCON1
        dc.w $0044,$ffff          ; BLTAFWM
        dc.w $0046,$ffff          ; BLTALWM
        dc.w $0064,$0000          ; BLTAMOD
        dc.w $0066,$0000          ; BLTDMOD
        dc.w $0050,(SRC+\4)>>16   ; BLTAPTH
        dc.w $0052,(SRC+\4)&$ffff ; BLTAPTL
        dc.w $0054,(DST+\4)>>16   ; BLTDPTH
        dc.w $0056,(DST+\4)&$ffff ; BLTDPTL
        dc.w (\1)<<8|$41,$fffe    ; WAIT line \1, inside the visible area
        dc.w $0180,\3             ; COLOR00: bar starts
        dc.w $0058,(10<<6)|2      ; BLTSIZE 10 rows x 2 words
        dc.w $0001,$7ffe          ; WAIT for the blitter to finish
        dc.w $0180,$0fff          ; COLOR00 white: BBUSY dropped
        dc.w $0180,$0000          ; COLOR00 black
        ENDM

; Eight consecutive lines of one blit class.
BAND    MACRO
        BLITLINE \1+0,\2,\3,\4
        BLITLINE \1+1,\2,\3,\4
        BLITLINE \1+2,\2,\3,\4
        BLITLINE \1+3,\2,\3,\4
        BLITLINE \1+4,\2,\3,\4
        BLITLINE \1+5,\2,\3,\4
        BLITLINE \1+6,\2,\3,\4
        BLITLINE \1+7,\2,\3,\4
        ENDM

        cnop 0,4
clist:
        dc.w $0100,$0200          ; BPLCON0: no planes, colour burst
        dc.w $0180,$0000          ; COLOR00 black
        BAND $40,$0000,$0fd3,0
        BAND $4c,$0002,$0f80,LAST
        BAND $58,$0010,$04c4,0
        BAND $64,$0008,$04cf,0
        BAND $70,$0012,$0c4c,LAST
        dc.w $ffff,$fffe
clistend:
