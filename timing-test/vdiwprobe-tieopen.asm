; Mid-frame DIWSTRT/DIWSTOP tie over an already-open vertical flop.
;
; A DIWSTRT.V == DIWSTOP.V tie only guarantees the window never OPENS: on
; a flop an earlier DIWSTRT match already set this frame, the tie-valued
; registers change nothing until the beam reaches the shared comparator
; line, where set and reset both match and reset wins. Rows scanned
; between the rewrite and that line keep fetching and displaying.
;
; A register-derived level test that reads any tie as closed blanks those
; rows even though bitplane DMA fetched them; the renderer must take the
; row's DMA capture (the Agnus flop's own history) as the vertical
; authority and use the level test only where nothing was fetched
; (vdiwprobe-empty pins that whole-frame-closed case).
;
; Copper events per frame:
;
;   top      DIWSTRT=$5081, DIWSTOP=$97C1 (open at 80, close at 151)
;   v112     DIWSTRT=$9781 (start = stop = 151: a tie armed over the
;            OPEN flop -- rows 112..150 must keep displaying, and the
;            tie still cannot re-open anything at or after 151)
;
; Expected settled render: border from the capture top down to 79,
; stripes over 80..150, border from 151 down (the tie-valued registers
; govern every remaining line of the frame and never open).
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh).
CUST    equ $dff000
PLANE   equ $40000
CLIST   equ $60000
FILLW   equ 8192

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        lea PLANE,a0
        move.w #FILLW-1,d0
.fp:    move.w #$ff00,(a0)+       ; 8-on/8-off stripes
        dbra d0,.fp

        ; ---- copper list ----
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$01800113,(a1)+   ; COLOR00 dark blue border/background
        move.l #$01820fff,(a1)+   ; COLOR01 white stripes
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE
        move.l #$00e20000,(a1)+
        move.l #$008e5081,(a1)+   ; DIWSTRT: arm open at 80
        move.l #$009097c1,(a1)+   ; DIWSTOP: arm close at 151
        move.l #$70010000+$fffe,(a1)+ ; WAIT v112 (flop open since 80)
        move.l #$008e9781,(a1)+   ; DIWSTRT: start = stop = 151 (tie over
                                  ; the open flop: rows 112..150 stay lit,
                                  ; 151+ stays border)
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
