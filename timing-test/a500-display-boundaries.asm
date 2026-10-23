; Paired single/dual playfield collision/clear and pointer landing probe.
; Rows0..15: early CLXDAT, single/dual x8 gaps;16..31: BPL2PTL race;
; rows32..47: CPU reads across line-end refresh (no bitplane DMA).
; Four raw words per four-line row: collision0, beam0, collision1, beam1.
; See amiga-hardware-probes references and PR810 review notes for HW evidence.
        org $30000
        lea $dff000,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea $40000,a0
        move.w #5119,d0
.bitmap: clr.w (a0)+
        dbra d0,.bitmap
        lea $50000,a0
        move.w #8191,d0
.source: clr.w (a0)+
        dbra d0,.source
        move.w #$ffff,$50000
        move.w #$2c81,$8e(a6)
        move.w #$f4c1,$90(a6)
        move.w #$0038,$92(a6)
        move.w #$00d0,$94(a6)
        clr.w $108(a6)
        clr.w $10a(a6)
        clr.w $102(a6)
        move.w #$00c3,$98(a6)
        lea $40000,a3
        moveq #0,d6
.trial:
        move.w #$7fff,$96(a6)
        move.w #$2200,d0
        btst #3,d6
        beq.s .mode
        ori.w #$0400,d0
.mode: move.w d0,$100(a6)
        move.l #$50000,$e0(a6)
        move.l #$50000,$e4(a6)
        move.w $0e(a6),d0
        lea $180(a6),a2
        cmpi.w #16,d6
        bcs.s .target
        cmpi.w #32,d6
        bcc.s .target
        lea $e6(a6),a2
.target:
        move.w #$402e,d7
        move.w #$8300,d5
        cmpi.w #32,d6
        bcs.s .clock
        move.w #$40e0,d7
        move.w #$8200,d5
.clock:
        move.w d6,d0
        andi.w #7,d0
        lsl.w #2,d0
        lea .observers(pc),a0
        movea.l 0(a0,d0.w),a1
.frame: move.w 6(a6),d0
        cmpi.w #$4000,d0
        bcc.s .frame
.sync: move.w 6(a6),d0
        andi.w #$fffe,d0
        cmp.w d7,d0
        bcs.s .sync
        jsr (a1)
        moveq #3,d7
.row:  move.w d0,(a3)
        move.w d1,2(a3)
        move.w d2,4(a3)
        move.w d3,6(a3)
        lea 40(a3),a3
        dbra d7,.row
        addq.w #1,d6
        cmpi.w #48,d6
        bne.w .trial
        move.w #$7fff,$96(a6)
        lea $60000,a0
        move.l #$01001200,(a0)+
        move.l #$01020000,(a0)+
        move.l #$01080000,(a0)+
        move.l #$010a0000,(a0)+
        move.l #$008e2c81,(a0)+
        move.l #$00902cc1,(a0)+
        move.l #$00920038,(a0)+
        move.l #$009400d0,(a0)+
        move.l #$00e00004,(a0)+
        move.l #$00e20000,(a0)+
        move.l #$01800113,(a0)+
        move.l #$01820fff,(a0)+
        move.l #$fffffffe,(a0)+
        move.l #$60000,$80(a6)
        move.w #0,$88(a6)
        move.w #$8380,$96(a6)
.halt: bra.s .halt
.observers:
        dc.l .sample0
        dc.l .sample1
        dc.l .sample2
        dc.l .sample3
        dc.l .sample4
        dc.l .sample5
        dc.l .sample6
        dc.l .sample7
.sample0:
        move.w d5,$96(a6)
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample1:
        move.w d5,$96(a6)
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample2:
        move.w d5,$96(a6)
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample3:
        move.w d5,$96(a6)
        nop
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample4:
        move.w d5,$96(a6)
        nop
        nop
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample5:
        move.w d5,$96(a6)
        nop
        nop
        nop
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample6:
        move.w d5,$96(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
.sample7:
        move.w d5,$96(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w #$1000,(a2)
        move.w $0e(a6),d0
        andi.w #1,d0
        move.w 6(a6),d1
        move.w $0e(a6),d2
        andi.w #1,d2
        move.w 6(a6),d3
        rts
