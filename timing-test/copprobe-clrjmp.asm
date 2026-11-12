; COPJMP double-strobe probe (CLR.W of the strobe address).
;
; A 68000 CLR.W reads its operand before writing it, and the COPJMP
; strobes decode the address alone, so CLR.W COPJMP1 strobes the Copper
; twice, four colour clocks apart. After a CPU strobe the Copper spends
; one cycle requesting the bus before it fetches from the new list, so
; the second strobe arrives before the first MOVE of that list has taken
; effect and simply restarts it.
;
; Each list A below begins by pointing COP1LC at its list B. With the
; request cycle the second strobe restarts list A (COP1LC was not written
; yet) and A paints its stripe GREEN. A Copper that fetches immediately
; lands the COP1LC MOVE between the two strobes, so the second one jumps
; to list B and the stripe is RED.
;
;   lines $40-$5F: strobe issued on line $20
;   lines $90-$AF: strobe issued on line $70, one colour clock later in
;                  the CPU/beam phase (an odd-length instruction in
;                  between), so both strobe parities are covered
;
; Static render: both stripes green.
CUST   equ $dff000

        lea CUST,a6
        move.w #$7fff,$9a(a6)     ; interrupts off
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)     ; all DMA off
        move.w #$0113,$180(a6)    ; background
        ; patch the absolute low words of the B lists into the A lists
        lea    listb1(pc),a0
        move.l a0,d0
        lea    lista1(pc),a1
        move.w d0,2(a1)
        lea    listb2(pc),a0
        move.l a0,d0
        lea    lista2(pc),a1
        move.w d0,2(a1)
        lea    idle(pc),a0
        move.l a0,$80(a6)         ; COP1LC = idle list
        move.w #$8280,$96(a6)     ; DMAEN|COPEN

frame:
        ; frame sync: wait for V8 to rise (line >= 256), then fall (wrap).
.f1:    move.l $04(a6),d0
        btst   #16,d0
        beq.s  .f1
.f2:    move.l $04(a6),d0
        btst   #16,d0
        bne.s  .f2

        move.w #$20,d2
        bsr.s  lwait
        lea    lista1(pc),a0
        move.l a0,$80(a6)         ; COP1LC = list A1
        clr.w  $88(a6)            ; COPJMP1: read strobe, then write strobe

        move.w #$70,d2
        bsr.s  lwait
        lea    lista2(pc),a0
        move.l a0,$80(a6)         ; COP1LC = list A2
        btst   #0,d0              ; 10 clocks: shifts the strobe one cck
        clr.w  $88(a6)

        move.w #$c0,d2
        bsr.s  lwait
        lea    idle(pc),a0
        move.l a0,$80(a6)         ; the vertical-blank restart idles
        bra.s  frame

; Spin until VHPOSR's V7-0 reads d2.
lwait:  move.w $06(a6),d0
        lsr.w  #8,d0
        cmp.b  d2,d0
        bne.s  lwait
        rts

        cnop 0,4
idle:   dc.w $ffff,$fffe

lista1: dc.w $0082,$0000          ; COP1LCL = list B1 (patched)
        dc.w $4001,$fffe          ; WAIT line $40
        dc.w $0180,$00f0          ; COLOR00 = green: list A was restarted
        dc.w $6001,$fffe          ; WAIT line $60
        dc.w $0180,$0113
        dc.w $ffff,$fffe
listb1: dc.w $4001,$fffe
        dc.w $0180,$0f00          ; COLOR00 = red: jumped to list B
        dc.w $6001,$fffe
        dc.w $0180,$0113
        dc.w $ffff,$fffe

lista2: dc.w $0082,$0000          ; COP1LCL = list B2 (patched)
        dc.w $9001,$fffe          ; WAIT line $90
        dc.w $0180,$00f0
        dc.w $b001,$fffe          ; WAIT line $B0
        dc.w $0180,$0113
        dc.w $ffff,$fffe
listb2: dc.w $9001,$fffe
        dc.w $0180,$0f00
        dc.w $b001,$fffe
        dc.w $0180,$0113
        dc.w $ffff,$fffe
