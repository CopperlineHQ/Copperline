; CIA timer output and Paula request regression: both CIAs, 0..15 NOP gaps.
; Four raw columns per row: INTREQR, PRB, ICR, beam. IRQs and DMA disabled
; while sampling. PB6 is timer-A pulse output; INTREQR must use the staged
; CIA-output/Paula-input path. No interrupt handler or STOP is involved.
; Model regression snapshot of A500-supported behavior, not a new HW golden.
; Assemble with vasm -m68000 -no-opt -Fbin.
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
        moveq #0,d6
.trial:
        lea $bfe001,a5
        cmpi.w #16,d6
        bcs.s .selected
        lea $bfd000,a5
.selected:
        move.b #0,$e00(a5)
        move.b #$7f,$d00(a5)
        move.b $d00(a5),d0
        move.w #$7fff,$9c(a6)
        move.b #$ff,$300(a5)
        move.b #$ff,$100(a5)
        move.b #3,$400(a5)
        move.b #0,$500(a5)
        move.b #$81,$d00(a5)
        move.w d6,d0
        andi.w #15,d0
        lsl.w #2,d0
        lea .observers(pc),a0
        movea.l 0(a0,d0.w),a2
.sync: move.w 6(a6),d0
        andi.w #$fffe,d0
        cmpi.w #$4020,d0
        bne.s .sync
        jsr (a2)
        moveq #7,d7
.row:  move.w d0,(a3)
        move.w d1,2(a3)
        move.w d2,4(a3)
        move.w d3,6(a3)
        lea 40(a3),a3
        dbra d7,.row
        addq.w #1,d6
        cmpi.w #32,d6
        bne.w .trial
        move.b #0,$e00(a5)
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
        dc.l .sample8
        dc.l .sample9
        dc.l .sample10
        dc.l .sample11
        dc.l .sample12
        dc.l .sample13
        dc.l .sample14
        dc.l .sample15
.sample0:
        move.b #$1b,$e00(a5)
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample1:
        move.b #$1b,$e00(a5)
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample2:
        move.b #$1b,$e00(a5)
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample3:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample4:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample5:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample6:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample7:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample8:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample9:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample10:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample11:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample12:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample13:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample14:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
.sample15:
        move.b #$1b,$e00(a5)
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        nop
        move.w $1e(a6),d0
        moveq #0,d1
        move.b $100(a5),d1
        moveq #0,d2
        move.b $d00(a5),d2
        move.w 6(a6),d3
        rts
