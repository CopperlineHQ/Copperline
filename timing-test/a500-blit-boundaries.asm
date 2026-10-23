; A500 blitter startup, BBUSY/BZERO and descending destination restore.
; Each 8-line row renders four raw words: DMACONR, first D, last D, beam.
; Rows 0..7: one-word D clear; 8..15: one-word A->D zero copy;
; rows16..23: descending two-word A->D, CPU restores final D after0..7 NOPs.
; Bits remain raw pixels, duplicated vertically; no textual conversion.
; Hardware evidence: amiga-hardware-probes PR810 destination/mask review record.
; This is a regression snapshot, not an electrical edge measurement.
        org $30000
        lea $dff000,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea $40000,a0
        move.w #5119,d0
.clear: clr.w (a0)+
        dbra d0,.clear
        lea $40000,a3
        move.w #$5555,d4
        lea $52000,a4
        moveq #0,d6
.trial:
        move.w #$7fff,$96(a6)
        clr.w $100(a6)
        clr.w $42(a6)
        move.w #$ffff,$44(a6)
        move.w #$ffff,$46(a6)
        clr.w $60(a6)
        clr.w $62(a6)
        clr.w $64(a6)
        clr.w $66(a6)
        move.l #$50000,$50(a6)
        move.l #$52000,$54(a6)
        clr.l $52000
        clr.l $50000
        move.w #$01ff,$40(a6)
        move.w #$0041,d5
        cmpi.w #8,d6
        bcs.s .configured
        move.w #$09f0,$40(a6)
        cmpi.w #16,d6
        bcs.s .configured
        move.w #2,$42(a6)
        move.l #$50002,$50(a6)
        move.l #$52002,$54(a6)
        move.l #$aaaaaaaa,$50000
        move.w #$0042,d5
.configured:
        move.w #$8640,$96(a6)
        move.w d6,d0
        andi.w #7,d0
        cmpi.w #16,d6
        bcs.s .index
        addq.w #8,d0
.index: lsl.w #2,d0
        lea .observers(pc),a0
        movea.l 0(a0,d0.w),a2
.sync: move.w 6(a6),d0
        andi.w #$fffe,d0
        cmpi.w #$4020,d0
        bne.s .sync
        jsr (a2)
.record:
        moveq #7,d7
.row:  move.w d0,(a3)
        move.w d1,2(a3)
        move.w d2,4(a3)
        move.w d3,6(a3)
        lea 40(a3),a3
        dbra d7,.row
        rept 128
        nop
        endr
        addq.w #1,d6
        cmpi.w #24,d6
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
        dc.l .restore0
        dc.l .restore1
        dc.l .restore2
        dc.l .restore3
        dc.l .restore4
        dc.l .restore5
        dc.l .restore6
        dc.l .restore7
.sample0:
        move.w d5,$58(a6)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample1:
        move.w d5,$58(a6)
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample2:
        move.w d5,$58(a6)
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample3:
        move.w d5,$58(a6)
        nop
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample4:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample5:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample6:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.sample7:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts

.restore0:
        move.w d5,$58(a6)
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore1:
        move.w d5,$58(a6)
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore2:
        move.w d5,$58(a6)
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore3:
        move.w d5,$58(a6)
        nop
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore4:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore5:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore6:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
.restore7:
        move.w d5,$58(a6)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w d4,(a4)
        move.w $02(a6),d0
        move.w $52000,d1
        move.w $52002,d2
        move.w 6(a6),d3
        rts
