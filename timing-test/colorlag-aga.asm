; AGA COLORxx output-latency probe.
;
; Every raster line receives the same five Copper-driven COLOR00 edges. Lisa
; applies them one hires pixel later than OCS/ECS Denise. The dense vertical
; comb makes a one-sample regression visible across the whole golden image.
; Calibrated against vAmigaTS Denise/Registers/COLOR/colorlag and its real
; A1200 reference.
CUST    equ $dff000
CLIST   equ $60000

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)
        move.w #$0008,$180(a6)

        lea CLIST,a1
        move.w #$40,d2
.line:  move.w d2,d3
        lsl.w #8,d3
        move.w d3,d4
        or.w #$0041,d4
        move.w d4,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01800f00,(a1)+
        move.w d3,d4
        or.w #$0061,d4
        move.w d4,(a1)+
        move.w #$fffe,(a1)+
        move.l #$0180000f,(a1)+
        move.w d3,d4
        or.w #$0081,d4
        move.w d4,(a1)+
        move.w #$fffe,(a1)+
        move.l #$018000f0,(a1)+
        move.w d3,d4
        or.w #$00a1,d4
        move.w d4,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01800fff,(a1)+
        move.w d3,d4
        or.w #$00c1,d4
        move.w d4,(a1)+
        move.w #$fffe,(a1)+
        move.l #$01800008,(a1)+
        addq.w #1,d2
        cmp.w #$e8,d2
        bne.w .line
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$080(a6)
        move.w d0,$088(a6)
        move.w #$8280,$096(a6)   ; DMAEN|COPEN
.loop:  bra.s .loop
