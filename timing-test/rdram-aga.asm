; AGA palette readback probe (BPLCON2.RDRAM).
;
; Three border bands are reconstructed only from COLOR05 reads:
;   $40..$7F  bank 2 -> RGB $142536
;   $80..$BF  the same entry after an ignored write while RDRAM is set
;   $C0..$FF  bank 5 -> RGB $7A8B9C
; BANK and LOCT therefore both have to address reads like writes, and the
; COLORxx window must be read-only while RDRAM is active. Cross-checked with
; vAmigaTS Denise/Registers/COLOR/rdram and its real-A1200 reference, and
; re-verified 2026-08-27 on vAmiga 5.0b1's A1200_2MB AGA setup: the same
; three bands in the same order, differing only on the two band-transition
; lines (198 of 202628 pixels, rows 38 and 166).  Compare the bands by
; colour correspondence, not by RGB: vAmiga runs a YUV brightness/contrast/
; saturation monitor model over the palette, so its readback bands come out
; as $001522/$637588 where Copperline's raw nibble replication gives
; $112233/$778899.
CUST    equ $dff000

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        ; Bank 2 COLOR05 = $142536.
        move.w #$4000,$106(a6)
        move.w #$0123,$18a(a6)
        move.w #$4200,$106(a6)
        move.w #$0456,$18a(a6)

        ; Bank 5 COLOR05 = $7A8B9C.
        move.w #$a000,$106(a6)
        move.w #$0789,$18a(a6)
        move.w #$a200,$106(a6)
        move.w #$0abc,$18a(a6)
        clr.w  $106(a6)
        clr.w  $180(a6)

frame:
        ; Wait for V8 to rise and then for the next frame wrap.
.f1:    move.l $004(a6),d0
        btst #16,d0
        beq.s .f1
.f2:    move.l $004(a6),d0
        btst #16,d0
        bne.s .f2

        move.w #$40,d2
        bsr.w lwait
        move.w #$0100,$104(a6)   ; RDRAM
        move.w #$4000,$106(a6)   ; bank 2, high nibbles
        move.w $18a(a6),d4
        move.w #$4200,$106(a6)   ; bank 2, low nibbles
        move.w $18a(a6),d5
        bsr.w show_color

        move.w #$80,d2
        bsr.w lwait
        move.w #$0100,$104(a6)
        move.w #$4000,$106(a6)
        move.w #$0fff,$18a(a6)   ; ignored while RDRAM is active
        move.w $18a(a6),d4
        move.w #$4200,$106(a6)
        move.w $18a(a6),d5
        bsr.w show_color

        move.w #$c0,d2
        bsr.w lwait
        move.w #$0100,$104(a6)
        move.w #$a000,$106(a6)   ; bank 5, high nibbles
        move.w $18a(a6),d4
        move.w #$a200,$106(a6)   ; bank 5, low nibbles
        move.w $18a(a6),d5
        bsr.w show_color

        move.w #$00,d2           ; V8=1 aliases line $100
.v8:    move.l $004(a6),d0
        btst #16,d0
        beq.s .v8
        clr.w $104(a6)
        clr.w $106(a6)
        clr.w $180(a6)
        bra.w frame

; Show the 24-bit value held in d4:d5 through bank-0 COLOR00.
show_color:
        clr.w  $104(a6)          ; writes enabled
        clr.w  $106(a6)          ; bank 0, high nibbles
        move.w d4,$180(a6)
        move.w #$0200,$106(a6)   ; bank 0, low nibbles
        move.w d5,$180(a6)
        clr.w  $106(a6)
        rts

lwait:  move.w $006(a6),d0
        lsr.w #8,d0
        cmp.b d2,d0
        bne.s lwait
        rts
