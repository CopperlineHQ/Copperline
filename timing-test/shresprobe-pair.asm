; SPDX-License-Identifier: GPL-3.0-or-later
; ECS Denise super-hires colour selection: pixel pairing and nibble split.
;
; ECS Denise (8373) cannot look a colour up at the super-hires dot rate.
; It combines two pixels into one 4-bit colour register index, on aligned
; groups of four super-hires pixels (one lo-res pixel), pairing across
; the group: pixel 0 with pixel 2 and pixel 1 with pixel 3, the later
; pixel giving the high bit pair,
;
;     index = (pixel[base + 2] & 3) * 4 + (pixel[base] & 3),  base = p & 1
;
; so two bitplanes reach COLOR00 to COLOR15. Of the chosen register the
; first sub-pixel of each hi-res cell shows the high bit pair of every RGB
; nibble and the second the low pair, each pair replicated over the
; nibble (0..3 -> $0/$5/$A/$F); a monitor shows their average.
;
; The probe runs two super-hires bitplanes (BPLCON0 $2240, DDF $3C/$D4,
; standard DIW) in seventeen flat bands, each repeating one four-pixel
; group across the line. Band k (lines $30 + 12k) repeats the pixels
; k&3, k&3, k>>2, k>>2, which the pairing turns into COLOR k on both
; sub-pixels. COLOR k is red (k&3)*5, green (k>>2)*5, blue 0: all
; nibbles $0/$5/$A/$F, whose bit pairs are equal, so the split leaves
; them whole and every band is one flat colour.
;
; Band 16 (lines $F0-$FB) repeats pixel 1 and reprograms COLOR05 to $C3C
; first: the pair reaches COLOR05 and splits it into $F0F and $0F0. The
; list restores COLOR05 to $550 above band 0.
;
; Expected on ECS Denise (A500+, A600, A3000):
;   bands 0-15: red stepping 0, $5, $A, $F every band and green stepping
;   every four bands, i.e. COLOR00..COLOR15 in order;
;   band 16: flat mid grey (the average of $F0F and $0F0), not magenta.
; A per-pixel lookup instead shows only red (COLOR k&3 beside COLOR k>>2,
; no green at all) and band 16 as COLOR01 dark red. Pairing with the
; group grid offset by two pixels would transpose the index (red and
; green swapped in the band order).
;
; Sources: vAmigaTS Denise/Modes/shres (A500+ photographs of shindex1-5,
; shspot1-16, shramp1-7 and the README's measured rule). Not
; cross-checked against vAmiga: its implementation of the rule
; (PixelEngine::colorizeShres) postdates the 5.1 release.

        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        ; Sixteen constant buffers of $400 bytes at $40000: buffer n is
        ; filled with the word n * $1111 (one 4-pixel nibble repeated).
        lea     $40000,a0
        moveq   #0,d1
.buf:   move.w  #$400/2-1,d0
.fill:  move.w  d1,(a0)+
        dbra    d0,.fill
        add.w   #$1111,d1
        bcc.s   .buf
        ; COLORk = red (k&3)*5, green (k>>2)*5.
        lea     $180(a6),a0
        moveq   #0,d1
.pal:   move.w  d1,d2
        and.w   #3,d2
        mulu    #$500,d2
        move.w  d1,d3
        lsr.w   #2,d3
        mulu    #$050,d3
        or.w    d3,d2
        move.w  d2,(a0)+
        addq.w  #1,d1
        cmp.w   #16,d1
        bne.s   .pal
        lea     $44000,a0
        lea     list(pc),a1
        move.w  #(list_end-list)/2-1,d0
.copy:  move.w  (a1)+,(a0)+
        dbra    d0,.copy
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$003c,$092(a6)
        move.w  #$00d4,$094(a6)
        clr.w   $102(a6)
        clr.w   $104(a6)
        move.w  #-160,$108(a6)
        move.w  #-160,$10a(a6)
        move.w  #$2240,$100(a6)  ; two super-hires planes, COLOR
        move.l  #$44000,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$8380,$096(a6)
.loop:  bra.s   .loop

list:   dc.w    $2b07,$fffe,$018a,$0550,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$0000 ; COLOR05 back to $550
        dc.w    $3007,$fffe,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$0000 ; band 0 -> COLOR00
        dc.w    $3c07,$fffe,$00e0,$0004,$00e2,$3000,$00e4,$0004,$00e6,$0000 ; band 1 -> COLOR01
        dc.w    $4807,$fffe,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$3000 ; band 2 -> COLOR02
        dc.w    $5407,$fffe,$00e0,$0004,$00e2,$3000,$00e4,$0004,$00e6,$3000 ; band 3 -> COLOR03
        dc.w    $6007,$fffe,$00e0,$0004,$00e2,$0c00,$00e4,$0004,$00e6,$0000 ; band 4 -> COLOR04
        dc.w    $6c07,$fffe,$00e0,$0004,$00e2,$3c00,$00e4,$0004,$00e6,$0000 ; band 5 -> COLOR05
        dc.w    $7807,$fffe,$00e0,$0004,$00e2,$0c00,$00e4,$0004,$00e6,$3000 ; band 6 -> COLOR06
        dc.w    $8407,$fffe,$00e0,$0004,$00e2,$3c00,$00e4,$0004,$00e6,$3000 ; band 7 -> COLOR07
        dc.w    $9007,$fffe,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$0c00 ; band 8 -> COLOR08
        dc.w    $9c07,$fffe,$00e0,$0004,$00e2,$3000,$00e4,$0004,$00e6,$0c00 ; band 9 -> COLOR09
        dc.w    $a807,$fffe,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$3c00 ; band 10 -> COLOR10
        dc.w    $b407,$fffe,$00e0,$0004,$00e2,$3000,$00e4,$0004,$00e6,$3c00 ; band 11 -> COLOR11
        dc.w    $c007,$fffe,$00e0,$0004,$00e2,$0c00,$00e4,$0004,$00e6,$0c00 ; band 12 -> COLOR12
        dc.w    $cc07,$fffe,$00e0,$0004,$00e2,$3c00,$00e4,$0004,$00e6,$0c00 ; band 13 -> COLOR13
        dc.w    $d807,$fffe,$00e0,$0004,$00e2,$0c00,$00e4,$0004,$00e6,$3c00 ; band 14 -> COLOR14
        dc.w    $e407,$fffe,$00e0,$0004,$00e2,$3c00,$00e4,$0004,$00e6,$3c00 ; band 15 -> COLOR15
        dc.w    $f007,$fffe,$018a,$0c3c,$00e0,$0004,$00e2,$3c00,$00e4,$0004,$00e6,$0000 ; band 16 -> COLOR05 = $C3C
        dc.w    $fc07,$fffe,$00e0,$0004,$00e2,$0000,$00e4,$0004,$00e6,$0000 ; below: COLOR00
        dc.w    $ffff,$fffe
list_end:
