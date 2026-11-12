; Copper WAIT vertical compare on line 255 probe.
;
; The Copper compares only eight vertical bits (V7-V0). On line 255 the
; low byte is $FF, the largest value it takes, so a WAIT for any other
; line that is evaluated while the beam is still on line 255 is already
; satisfied and releases at once. That is why a list reaches lines 256
; and up with WAIT $FFDF,$FFFE first: it holds the Copper to the end of
; line 255, so the next WAIT is evaluated after the low byte has wrapped
; to $00 (Hardware Reference Manual, Copper chapter).
;
; The list runs both forms in one frame, on a plain COLOR00 background
; (no bitplanes, no sprites):
;
;   line $F0   grey      reference band, WAIT $F001
;   line $F8   dark      WAIT $F801
;   line $FF   red       WAIT $FF07 then WAIT $0801: the second WAIT is
;                        evaluated on line 255 ($FF >= $08) and releases
;                        at once, so red starts part way along line 255
;   line $110  green     WAIT $FFDF then WAIT $1001: evaluated after the
;                        wrap, waits for line 256+16
;   line $120  dark      WAIT $2001
;
; A model that holds a low-half WAIT on line 255 until the low byte wraps
; starts red on line $108 instead (and never reaches green: the following
; WAIT $FFDF is then evaluated on line $108, where the low byte never
; gets back to $FF before the frame ends, so red runs to the bottom).
;
; vAmiga agrees with the hardware rule: red from line 255, green from
; line $110.
CUST    equ $dff000
CLIST   equ $60000
DARK    equ $0013
GREY    equ $0333
RED     equ $0f00
GREEN   equ $00f0

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        move.w #$0200,$100(a6)    ; BPLCON0: no planes, colour on
        move.w #$0000,$106(a6)    ; BPLCON3: defaults, border shows COLOR00
        move.w #DARK,$180(a6)

        lea CLIST,a1
        lea list(pc),a0
        moveq #(listend-list)/4-1,d0
.copy:  move.l (a0)+,(a1)+
        dbra d0,.copy

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8280,$96(a6)     ; DMAEN|COPEN only
.l:     bra.s .l

list:   dc.w $0100,$0200          ; BPLCON0
        dc.w $0102,$0000          ; BPLCON1
        dc.w $0104,$0000          ; BPLCON2
        dc.w $0106,$0000          ; BPLCON3
        dc.w $0180,DARK
        dc.w $f001,$fffe          ; line $F0: reference band
        dc.w $0180,GREY
        dc.w $f801,$fffe          ; line $F8
        dc.w $0180,DARK
        dc.w $ff07,$fffe          ; line 255, early in the line
        dc.w $0801,$fffe          ; compared on line 255: $FF >= $08
        dc.w $0180,RED
        dc.w $ffdf,$fffe          ; hold to the end of line 255
        dc.w $1001,$fffe          ; compared after the wrap: line $110
        dc.w $0180,GREEN
        dc.w $2001,$fffe          ; line $120
        dc.w $0180,DARK
        dc.w $ffff,$fffe
listend:
