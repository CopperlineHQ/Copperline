; Vertical display window flop across interlaced fields probe.
;
; Agnus gates the display vertically with a set/reset flop (see
; vdiwprobe-flop): SET when the beam line matches DIWSTRT.V, RESET when
; it matches DIWSTOP.V, and the frame's last raster line forces a reset
; so a window whose DIWSTOP line the beam never reaches stays open only
; to the end of its own frame. Under LACE the frames alternate: a long
; field (LOF=1) runs one line more than a short one (PAL 313/312 lines,
; NTSC 263/262). The forced reset follows the field being scanned: a
; short field resets on its own last line (PAL 311, NTSC 261), not only
; on the long field's (312/262).
;
; The window here stops at line 312, the long field's last line, so the
; short field never reaches its DIWSTOP. With the reset on the short
; field's last line both fields open at their own DIWSTRT. A model that
; resets only on the fixed standard line carries the short field's open
; window across the wrap: the following long field fetches from the top
; of the frame, one field shifted against the other. That is what wove
; the Video Toaster 4.2 switcher's interlaced screen with its fields
; swapped (NTSC, DIWSTOP $06D1 = line 262): its planes hold exactly 481
; rows, the 241 lines of the long field (21..261) and the 240 of the
; short field (21..260), so the short field cannot display its line 261
; on the hardware the switcher was written for.
;
; Display: one bitplane of 8-on/8-off stripes, LACE, the same Copper
; list every field (field-blind, so the capture's field parity does not
; matter). COLOR01 is red above the window's own DIWSTRT (only a window
; carried open from the previous field shows it), white over the window,
; yellow from line 300 to the bottom.
;
;   top      DIWSTRT=$4081 (open at 64), DIWSTOP=$38C1 (stop at 312)
;   v64      COLOR01 white
;   v300     COLOR01 yellow
;
; Expected settled render, whichever field is captured: border from the
; capture top down to line 63 (no red), white stripes over 64..299,
; yellow stripes from 300; a short field shows border on its last line
; 311, a long field yellow on 311 and border on 312. A carried-open model
; lights red stripes over the long field's top instead, and yellow on
; the short field's line 311.
;
; vAmiga is not a valid reference here: it force-resets on the fixed
; standard line only, the carried-open model this probe replaces.
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
        move.l #$01001204,(a1)+   ; BPLCON0: 1 plane, LACE
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$01800113,(a1)+   ; COLOR00 dark blue border/background
        move.l #$01820f00,(a1)+   ; COLOR01 red: rows above DIWSTRT
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE
        move.l #$00e20000,(a1)+
        move.l #$008e4081,(a1)+   ; DIWSTRT: open at 64
        move.l #$009038c1,(a1)+   ; DIWSTOP: stop at 312 (V8 = !V7),
                                  ; reached by the long field only
        move.l #$40010000+$fffe,(a1)+ ; WAIT v64
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$ffdffffe,(a1)+   ; WAIT v255 (cross the V8 boundary)
        move.l #$2c010000+$fffe,(a1)+ ; WAIT v300
        move.l #$01820ff0,(a1)+   ; COLOR01 yellow
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
