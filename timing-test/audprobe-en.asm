; AUDxEN disable/re-enable state-machine probe (issue #74 class).
;
; Audio is not render-visible, but the per-word AUD0 interrupt cadence is:
; AUD0 plays a silent one-word wave (AUD0LEN=1) at PER=227 (one sample per
; PAL line): the buffer cycle restarts every word, so INTREQ.AUD0 fires
; every two lines while DMA plays, and the free-running IRQ-mode tick
; continues at the same rate when DMA is off. The CPU walks 96
; beam-synced lines, polling INTREQR each line and drawing a mark on that
; line's strip row when the interrupt was seen (then clearing it); a left
; reference column marks every observed line. DMACON is scripted inside
; the strip:
;
;   line +0:   SET AUD0EN            (cadence establishes: marks every 2)
;   line +16:  CLR AUD0EN            } the issue #74 punch: a disable
;   line +17:  SET AUD0EN            } re-enabled within the same word.
;                                    The deferred word-boundary disable
;                                    must not impose a period-scaled dead
;                                    time on the re-enable; a regression
;                                    shifts or stalls the following marks.
;   line +40:  CLR AUD0EN            (deferred disable: marks stop at the
;                                    word boundary)
;   line +56:  SET AUD0EN            (cold restart latency and cadence)
;
; The Paula HRM FSM rework regressed the punch case once (re-fixed with
; the deferred AUDxEN-disable sampled at the word boundary); this strip
; pins the full sequence. NOTE: the issue #74 analysis found vAmiga
; handles the punch case worse than Copperline, so this golden is pinned
; to Copperline's issue-validated behaviour, not cross-checked.
CUST   equ $dff000
BMP    equ $40000
WAVE   equ $50000
CLIST  equ $60000

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #10240-1,d0
.cz:    clr.w (a0)+
        dbra d0,.cz
        lea WAVE,a0
        move.w #64-1,d0
.cw:    clr.w (a0)+
        dbra d0,.cw
        lea CLIST,a1
        move.l #$01001200,(a1)+   ; BPLCON0: 1 plane lo-res
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD
        move.l #$010a0000,(a1)+   ; BPL2MOD
        move.l #$01800113,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$008e2c81,(a1)+   ; DIWSTRT
        move.l #$00902cc1,(a1)+   ; DIWSTOP
        move.l #$00e00004,(a1)+   ; BPL1PT = $40000
        move.l #$00e20000,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN

        ; AUD0: silent 64-word wave, one sample per line
        move.l #WAVE,$a0(a6)      ; AUD0LC
        move.w #1,$a4(a6)         ; AUD0LEN=1: cycle IRQ every word
        move.w #227,$a6(a6)       ; AUD0PER
        move.w #0,$a8(a6)         ; AUD0VOL

        ; frame sync: wait for V8 to rise, then fall
.f1:    move.l $04(a6),d0
        btst   #16,d0
        beq.s  .f1
.f2:    move.l $04(a6),d0
        btst   #16,d0
        bne.s  .f2

        moveq  #0,d2              ; strip index 0..95
        move.w #$50,d3            ; beam line of strip row 0
        lea    BMP+40*40,a4       ; strip row 0 = bitmap row 40
.strip: bsr.s  lwait
        ; scripted DMACON actions
        cmp.w  #0,d2
        bne.s  .n0
        move.w #$8001,$96(a6)     ; SET AUD0EN
.n0:    cmp.w  #16,d2
        bne.s  .n1
        move.w #$0001,$96(a6)     ; CLR AUD0EN (punch)
.n1:    cmp.w  #17,d2
        bne.s  .n2
        move.w #$8001,$96(a6)     ; SET AUD0EN (within the same word)
.n2:    cmp.w  #40,d2
        bne.s  .n3
        move.w #$0001,$96(a6)     ; CLR AUD0EN
.n3:    cmp.w  #56,d2
        bne.s  .n4
        move.w #$8001,$96(a6)     ; SET AUD0EN (cold restart)
.n4:
        ; observe INTREQR.AUD0 for this line
        move.w #$f000,(a4)        ; reference column
        move.w $1e(a6),d0
        btst   #7,d0
        beq.s  .nomark
        move.w #$ffff,8(a4)       ; interrupt mark (word 4)
        move.w #$0080,$9c(a6)     ; clear INTREQ.AUD0
.nomark:
        lea    40(a4),a4
        addq.w #1,d3
        addq.w #1,d2
        cmp.w  #96,d2
        bne.s  .strip
.halt:  bra.s  .halt

; Spin until VHPOSR's V7-0 reads d3.
lwait:  move.w $06(a6),d0
        lsr.w  #8,d0
        cmp.b  d3,d0
        bne.s  lwait
        rts
