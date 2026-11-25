; Sample Paula's transmit shifter at two instants after writing SERDAT.
; Upper band sends a framed NUL ($0100). Lower band writes $0000.
; Red = busy at both samples, amber = busy only at the first,
; green = idle at both, magenta = idle then busy.
        lea $dff000,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)
        move.w #$01ff,$032(a6)
        move.w #$0800,$09e(a6)
        move.w #$0200,$100(a6)
        move.w #$0100,d0
        bsr.s sample
        lea upper(pc),a0
        move.w d1,(a0)
        moveq #0,d0
        bsr.s sample
        lea lower(pc),a0
        move.w d1,(a0)
        lea list(pc),a0
        move.l a0,$080(a6)
        move.w d0,$088(a6)
        move.w #$8280,$096(a6)
.forever: bra.s .forever
sample:
.ready: move.w $018(a6),d1
        btst #12,d1
        beq.s .ready
        move.w d0,$030(a6)
        nop
        nop
        nop
        nop
        move.w $018(a6),d3
        moveq #3,d2
.line:  move.b $006(a6),d4
.wait:  cmp.b $006(a6),d4
        beq.s .wait
        dbra d2,.line
        move.w $018(a6),d4
        moveq #0,d1
        btst #12,d3
        sne d1
        and.w #1,d1
        add.w d1,d1
        btst #12,d4
        beq.s .color
        addq.w #1,d1
.color: add.w d1,d1
        lea colors(pc),a1
        move.w 0(a1,d1.w),d1
        rts
colors: dc.w $f00,$fa0,$f0f,$0f0
list:   dc.w $0180,0,$4001,$fffe,$0180
upper:  dc.w 0
        dc.w $9001,$fffe,$0180
lower:  dc.w 0
        dc.w $e001,$fffe,$0180,0,$ffff,$fffe
