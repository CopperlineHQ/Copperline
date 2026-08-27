; Vertical display window set/reset flop probe.
;
; Agnus gates the display vertically with a flop: SET when the beam line
; matches DIWSTRT.V, RESET when it matches DIWSTOP.V (reset wins a tie).
; The comparators run against the live registers at line starts and after
; DIW writes; a match on any other line leaves the flop alone. Two
; consequences a level comparison (vstart <= v < vstop) gets wrong:
;
;   - Rewriting DIWSTOP after its old value already matched does NOT
;     re-open the window: the flop is closed and a later DIWSTOP match on
;     a closed flop is a no-op. (The Kang Fu CD32 screen-split regression
;     class: close the game screen at line N, program the status bar's
;     window and pointers during N, let the new DIWSTRT open it at N+1.
;     The level test resumed fetching in the tail of line N and sheared
;     the bar's planes.)
;   - A window whose DIWSTOP line the beam never reaches (VSTOP values
;     below 128 address lines 256-383, the V8 convention) stays open only
;     to the end of its own frame: the video standard's fixed line 312
;     (PAL) / 262 (NTSC) forces a reset, so the display runs into the
;     bottom border but the next frame's top starts closed until DIWSTRT
;     matches again. The comparator is the fixed standard line, as vAmiga
;     models it (an interlaced short field ends at 311 and is not
;     force-reset; this progressive probe cannot separate the two, the
;     interlace unit test pins the choice). Copperline originally carried
;     the flop across the blank and lit the whole top of the frame, which
;     this probe caught.
;
; Display: one bitplane of 8-on/8-off stripes; open rows show the stripe
; field, closed rows show the border. Copper events per frame:
;
;   top      DIWSTOP=$84C1 (stop 132 armed against a closed flop: no-op)
;            DIWSTRT=$9081 (arm open at 144)
;   v134     DIWSTOP=$A0C1 (arm close at 160)          -> band 144..159
;   v162     DIWSTOP=$B0C1 (old stop already matched: a stop rewrite
;            must NOT reopen -- rows 160..175 stay border)
;   v178     DIWSTRT=$C081, DIWSTOP=$C0C1 (set and reset both match at
;            192: reset wins the tie -- rows stay border)
;   v200     DIWSTRT=$D081, DIWSTOP=$E0C1               -> band 208..223
;   v232     DIWSTRT=$F081, DIWSTOP=$3CC1 (open at 240; stop line 316 is
;            never reached -> open to the fixed reset line 312, where the
;            forced reset ends it; the top of the frame stays border)
;
; Expected settled render: border from the capture top down to 143,
; stripes over 144..159, 208..223 and 240..311, border over the gaps and
; over line 312.
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
        move.l #$009084c1,(a1)+   ; DIWSTOP: stop 132 armed, flop closed
        move.l #$008e9081,(a1)+   ; DIWSTRT: arm open at 144
        move.l #$86010000+$fffe,(a1)+ ; WAIT v134
        move.l #$0090a0c1,(a1)+   ; DIWSTOP: arm close at 160
        move.l #$a2010000+$fffe,(a1)+ ; WAIT v162
        move.l #$0090b0c1,(a1)+   ; DIWSTOP rewrite behind a closed flop:
                                  ; must NOT reopen 160..175
        move.l #$b2010000+$fffe,(a1)+ ; WAIT v178
        move.l #$008ec081,(a1)+   ; DIWSTRT: arm open at 192
        move.l #$0090c0c1,(a1)+   ; DIWSTOP: arm close at 192 (tie ->
                                  ; reset wins, stays border)
        move.l #$c8010000+$fffe,(a1)+ ; WAIT v200
        move.l #$008ed081,(a1)+   ; DIWSTRT: arm open at 208
        move.l #$0090e0c1,(a1)+   ; DIWSTOP: arm close at 224
        move.l #$e8010000+$fffe,(a1)+ ; WAIT v232
        move.l #$008ef081,(a1)+   ; DIWSTRT: arm open at 240
        move.l #$00903cc1,(a1)+   ; DIWSTOP: stop line 316 never reached
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
