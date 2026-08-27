; COPJMP read-strobe probe (Copper list flip by TST.W of the strobe).
;
; The COPJMP1/COPJMP2 strobes decode the register address alone: a CPU
; READ of $DFF088 reloads the Copper PC from COP1LC exactly as a write
; does (reading a write-only register performs a bus write of the
; floating data-bus residue into it). Vertical-blank handlers use this
; (move.l #list,COP1LC ; tst.w COPJMP1) to make a double-buffer list
; swap take effect in the SAME frame, before display fetch begins.
;
; Static render: COP1LC holds list A (red stripe, lines $60..$9F) across
; every vertical-blank restart. Each frame at line $20 the CPU sets
; COP1LC to list B (green stripe, same lines), strobes COPJMP1 with a
; TST.W (a read), then restores COP1LC to list A before the next
; restart:
;   - the read fires the strobe (real hardware): list B paints the
;     stripe -> GREEN;
;   - the read is inert (wrong model): list A keeps running -> RED.
; Cross-checked against vAmiga, whose write-only-register read model
; performs the strobe.
CUST   equ $dff000

        lea CUST,a6
        move.w #$7fff,$9a(a6)     ; interrupts off
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)     ; all DMA off
        move.w #$0113,$180(a6)    ; background
        lea    lista(pc),a0
        move.l a0,$80(a6)         ; COP1LC = list A
        move.w #$8280,$96(a6)     ; DMAEN|COPEN: list A runs from the
                                  ; next vertical-blank restart

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
        lea    listb(pc),a0
        move.l a0,$80(a6)         ; COP1LC = list B
        tst.w  $88(a6)            ; READ of COPJMP1: the jump under test
        lea    lista(pc),a0
        move.l a0,$80(a6)         ; latch list A for the vblank restart
        bra.s  frame

; Spin until VHPOSR's V7-0 reads d2.
lwait:  move.w $06(a6),d0
        lsr.w  #8,d0
        cmp.b  d2,d0
        bne.s  lwait
        rts

        cnop 0,4
lista:  dc.w $6001,$fffe          ; WAIT line $60
        dc.w $0180,$0f00          ; COLOR00 = red: the strobe did NOT fire
        dc.w $a001,$fffe          ; WAIT line $A0
        dc.w $0180,$0113          ; background below the stripe
        dc.w $ffff,$fffe
listb:  dc.w $6001,$fffe          ; WAIT line $60
        dc.w $0180,$00f0          ; COLOR00 = green: the read strobed
        dc.w $a001,$fffe          ; WAIT line $A0
        dc.w $0180,$0113          ; background below the stripe
        dc.w $ffff,$fffe
