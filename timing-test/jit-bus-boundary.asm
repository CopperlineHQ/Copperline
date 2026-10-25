; JIT native-prefix/MMIO ordering probe, 68020+ accelerator model.
; Copy the loop to configured fast RAM, then draw two native-delay markers
; per four-line band. A stale chipset clock at COLOR00 writes collapses or
; shifts the green marker. No program identity or ROM behavior is involved.
; vAmiga is not a timing reference for this probe: it has no matching JIT
; one-clock-per-instruction accelerator model. Unit tests compare the same
; instruction stream at single-instruction and deadline-bounded schedules.
        lea $dff000,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)
        move.w #$0200,$100(a6)
        move.w #0,$106(a6)
        move.w #$0013,$180(a6)
        lea resident(pc),a0
        lea $00201000,a1
        move.w #(resident_end-resident)/2-1,d0
.copy: move.w (a0)+,(a1)+
        dbra d0,.copy
        jmp $00201000
resident:
.f1:   move.l $04(a6),d0
        btst #16,d0
        beq.s .f1
.f2:   move.l $04(a6),d0
        btst #16,d0
        bne.s .f2
        move.w #$40,d1
.band: move.w $06(a6),d0
        lsr.w #8,d0
        cmp.b d1,d0
        bne.s .band
        move.w #$0013,$180(a6)
        move.w #255,d0
.delay1:
        addq.l #1,d2
        dbra d0,.delay1
        move.w #$00f0,$180(a6)
        moveq #63,d0
.delay2:
        addq.l #1,d2
        dbra d0,.delay2
        move.w #$0013,$180(a6)
        addq.w #4,d1
        cmp.w #$c0,d1
        bne.s .band
        bra.s .f1
resident_end:
