; INTENA unmask-window probe (border-colour stripes).
;
; A request can sit pending in INTREQ while the program exposes it to the
; CPU only for a moment: unmask the source in INTENA, branch, mask it
; again. Wait loops are written this way so that the handler can only run
; at one point of the loop. The window is two instructions long: about
; five colour clocks on a 68000, about three on a 68EC020 running the
; loop from its instruction cache.
;
; Paula drives the IPL pins from a registered copy of INTREQ & INTENA, so
; the mask write reaches the CPU no sooner than the unmask write did. A
; window shorter than the recognition delay still arrives, as a pulse of
; the same width after the mask write has landed, and a 68020 holds a
; request it has sampled until the next instruction boundary.
;
; The probe makes six passes. Each one raises DSKBLK in INTREQ with a CPU
; write, lets it settle while masked, then runs the loop
;
;       move.w  #$0002,$9a(a6)      ; mask
;       (has the handler counted? leave)
;       move.w  #$8002,$9a(a6)      ; unmask
;       bra.s   loop
;
; for at most 999 turns. The level 1 handler acknowledges the request and
; counts. With display DMA off the raster shows COLOR00:
;
;   lines $40..$4F: white reference stripe
;   lines $50..$DF: one stripe per pass, 24 lines each: green = the
;                   handler ran from inside the window, red = 999 windows
;                   opened and closed without it
;   line  $E0:      back to the dark background
;
; Expected on every machine: six green stripes. Runs on the 68000 OCS
; machine and on the 68EC020 AGA machine in the golden suite. vAmiga 4.4
; (A500_OCS_1MB) and vAmiga 5.0b1 (A1200_2MB) render six green stripes.
;
; Regression example: the CD32 Classic Lotus Trilogy waits this way for a
; Copper-set DSKBLK request at its race screen.
CUST          equ $dff000
LVOSuperState equ -150

        move.w  296(a6),d7          ; ExecBase->AttnFlags, while the OS is alive
        jsr     LVOSuperState(a6)   ; supervisor mode for good
        move.w  #$2700,sr
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)
        move.w  #$7fff,$9c(a6)
        move.w  #$7fff,$96(a6)
        move.w  #$0113,$180(a6)     ; background

        btst    #0,d7               ; 68010+: vectors at zero
        beq.s   .cpuok
        moveq   #0,d0
        dc.w    $4e7b,$0801         ; movec d0,vbr
        btst    #1,d7               ; 68020+: run the loop from the cache
        beq.s   .cpuok
        moveq   #$09,d0             ; CACR: enable and clear the icache
        dc.w    $4e7b,$0002         ; movec d0,cacr
.cpuok:
        lea     handler(pc),a0
        move.l  a0,$64.w            ; level 1 autovector
        lea     count(pc),a5
        clr.w   (a5)
        lea     stripes(pc),a4
        move.w  #$c000,$9a(a6)      ; master enable only
        move.w  #$2000,sr
        moveq   #5,d7               ; six passes

pass:   move.w  #$8002,$9c(a6)      ; request DSKBLK: pending, masked
        moveq   #63,d0
.settle dbra    d0,.settle          ; well past any recognition delay
        move.w  (a5),d5
        move.w  #999,d6
loop:   move.w  #$0002,$9a(a6)      ; mask
        move.w  (a5),d0
        cmp.w   d5,d0
        bne.s   got
        subq.w  #1,d6
        beq.s   missed
        move.w  #$8002,$9a(a6)      ; unmask
        bra.s   loop
got:    move.w  #$00f0,(a4)+
        bra.s   next
missed: move.w  #$0f00,(a4)+
        move.w  #$0002,$9c(a6)      ; drop the request nobody saw
next:   dbra    d7,pass
        move.w  #$7fff,$9a(a6)

frame:
        ; frame sync: wait for V8 to rise (line >= 256), then fall (wrap).
.f1:    move.l  $04(a6),d0
        btst    #16,d0
        beq.s   .f1
.f2:    move.l  $04(a6),d0
        btst    #16,d0
        bne.s   .f2

        move.w  #$40,d2
        bsr.s   lwait
        move.w  #$0fff,$180(a6)     ; reference stripe
        lea     stripes(pc),a1
        move.w  #$50,d2
        moveq   #5,d7
.bar:   bsr.s   lwait
        move.w  (a1)+,$180(a6)
        add.w   #$18,d2
        dbra    d7,.bar
        bsr.s   lwait               ; d2 = $E0
        move.w  #$0113,$180(a6)     ; background below the stripes
        bra.s   frame

; Spin until VHPOSR's V7-0 reads d2.
lwait:  move.w  $06(a6),d0
        lsr.w   #8,d0
        cmp.b   d2,d0
        bne.s   lwait
        rts

; Level 1: acknowledge DSKBLK and count.
handler:
        move.w  #$0002,CUST+$9c
        move.l  a0,-(sp)
        lea     count(pc),a0
        addq.w  #1,(a0)
        move.l  (sp)+,a0
        rte

count:   dc.w 0
stripes: dcb.w 6,0
