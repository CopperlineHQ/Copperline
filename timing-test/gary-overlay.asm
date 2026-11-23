; SPDX-License-Identifier: GPL-3.0-or-later
; A500 with 512 KiB trapdoor RAM, or A2000 with RAM at $C00000.
; Relocate the complete test outside the overlay, including all instructions
; between PA0 rising and falling. No stack or vectors in the overlay are used.
; Dark green = four identical ROM windows and the underlying RAM survived.
; Red = overlay decode failed. vAmiga 5 also displays red: its CPU map
; overlays eight 64 KiB banks. The Gary replacement's ROM select covers all
; 32 banks. This probe is intended to distinguish those decodes on a board.
        jsr     -150(a6)         ; exec.library/SuperState
        move.w  #$2700,sr
        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$096(a6)
        clr.w   $100(a6)
        lea     probe(pc),a0
        lea     $c10000,a1
        move.w  #(done-probe)/2-1,d0
.copy:  move.w  (a0)+,(a1)+
        dbra    d0,.copy
        jmp     $c10000

probe:  lea     $bfe001,a0
        bset    #0,$200(a0)      ; DDRA0 drives OVL
        bclr    #0,(a0)
        lea     $4000,a1
        move.l  #$19234567,(a1)
        move.l  $f84000,d1
        bset    #0,(a0)
        move.w  #$0f00,d2
        cmp.l   (a1),d1
        bne.s   .off
        cmp.l   $84000,d1
        bne.s   .off
        cmp.l   $104000,d1
        bne.s   .off
        cmp.l   $184000,d1
        bne.s   .off
        move.l  #$deadbeef,$184000
        move.w  #$02c4,d2
.off:   bclr    #0,(a0)
        cmpi.l  #$19234567,(a1)
        beq.s   .paint
        move.w  #$0f00,d2
.paint: move.w  d2,$180(a6)
.hold:  bra.s   .hold
done:
