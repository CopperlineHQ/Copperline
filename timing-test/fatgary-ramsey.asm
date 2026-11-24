; SPDX-License-Identifier: GPL-3.0-or-later
; A3000/A4000. Run from a cold boot of the probe disk. The CPU executes in
; chip RAM. Change only Ramsey's refresh rate, preserving fitted DRAM modes.
; Green = both register mirrors and the external interrupt gate passed, and
; Ramsey accepted a mode write and restoration. Red = a check failed.
; Yellow = the interrupt latch answered at offset $09A but not elsewhere in
; its four pages.
        machine 68040
        move.w  296(a6),d7       ; ExecBase AttnFlags
        jsr     -150(a6)         ; exec.library/SuperState
        move.w  #$2700,sr
        ; The boot ROM may leave an MMU mapping which omits shadow pages.
        ; Execute from identity-mapped chip RAM and use our own stack/vectors.
        lea     $60000,sp
        moveq   #0,d0
        btst    #3,d7            ; 68040
        beq.s   .m030
        cpusha  bc
        movec   d0,cacr
        movec   d0,tc
        bra.s   .physical
.m030:  machine 68030
        movec   d0,cacr
        clr.l   -(sp)
        pmove   (sp),tc
        addq.l  #4,sp
.physical:
        lea     $dff000,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)
        move.w  #$0200,$100(a6)
        clr.w   $106(a6)
        move.w  #$0f00,$180(a6)
        move.w  #$8004,$dfc09a
        btst    #2,$cff01d
        beq.s   fail
        move.w  #$0004,$c1009a
        btst    #2,$01d(a6)
        bne.s   fail
        ; Install an autovector in chip RAM, then test a level 1
        ; software interrupt. The inhibited interval spans many CPU polls.
        move.l  #$61000,d0
        movec   d0,vbr
        movea.l d0,a1
        lea     irq(pc),a0
        move.l  a0,$64(a1)
        moveq   #0,d6
        move.w  #0,$df809a
        move.w  #$c004,$09a(a6)
        move.w  #$8004,$09c(a6)
        move.w  #$2000,sr
        move.w  #255,d0
.wait:  dbra    d0,.wait
        tst.w   d6
        bne.s   fail
        move.w  #$8000,$dfb09a
        move.w  #255,d0
.poll:  tst.w   d6
        bne.s   second
        dbra    d0,.poll
fail:   bra.s   fail
irq:    addq.w  #1,d6
        move.w  #4,$09c(a6)
        rte
        ; Gary has no A2-A11 pins: the latch must answer at any offset of
        ; $DF8000-$DFBFFF, not only the documented $09A.
second: move.w  #$2700,sr
        moveq   #0,d6
        move.w  #0,$df9180
        move.w  #$8004,$09c(a6)
        move.w  #$2000,sr
        move.w  #255,d0
.wait:  dbra    d0,.wait
        tst.w   d6
        bne.s   docfail
        move.w  #$8000,$dfa000
        move.w  #255,d0
.poll:  tst.w   d6
        bne.s   ramsey
        dbra    d0,.poll
        bra.s   fail
docfail: move.w #$2700,sr
        move.w  #$0ff0,$180(a6)
.hold:  bra.s   .hold
ramsey: move.w  #$2700,sr
        lea     $de0003,a0
        move.b  (a0),d1
        move.b  d1,d2
        and.b   #$9f,d2
        cmp.b   d1,d2
        bne.s   .set
        or.b    #$20,d2
.set:   move.b  d2,(a0)
        move.w  #255,d0
.latch: cmp.b   (a0),d2
        beq.s   .restore
        dbra    d0,.latch
        bra     fail
.restore: move.b d1,(a0)
        move.w  #255,d0
.check: cmp.b   (a0),d1
        beq.s   .ok
        dbra    d0,.check
        bra     fail
.ok:    move.w  #$02c4,$180(a6)
.hold:  bra.s   .hold
