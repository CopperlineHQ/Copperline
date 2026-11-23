; Super Buster II: the CPU can see the head of a chip-RAM DMA copy while
; its tail is still pending. The stock A4000 probe config fits one A4091
; as the first Z3 card ($40000000); no disk or proprietary ROM is needed.
; Green = partial progress observed and every longword verified. Red = a
; failed check, amber = waiting for DMA. vAmiga lacks an A4091 reference.
CUST    equ $dff000
NCR     equ $40800000
SCRIPT  equ $50000
SRC     equ $60000
DST     equ $64000
WORDS   equ 1024

        lea     CUST,a6
        move.w  #$7fff,$9a(a6)
        move.w  #$7fff,$96(a6)
        move.w  #$0200,$100(a6)
        clr.w   $106(a6)
        move.w  #$fa0,$180(a6)
        lea     SRC,a0
        lea     DST,a1
        move.w  #WORDS-1,d0
.init:  move.l  #$42555354,(a0)+
        clr.l   (a1)+
        dbra    d0,.init
        lea     SCRIPT,a0
        move.l  #$c0000000+WORDS*4,(a0)+
        move.l  #SRC,(a0)+
        move.l  #DST,(a0)+
        move.l  #$98080000,(a0)+
        clr.l   (a0)
        lea     NCR,a2
        move.l  #SCRIPT,$2c(a2)
.head:  cmpi.l  #$42555354,DST
        bne.s   .head
        tst.l   DST+4*(WORDS-1)
        bne.s   .fail
.done:  btst    #0,$22(a2)     ; ISTAT.DIP (SCRIPTS INT)
        beq.s   .done
        btst    #2,$0f(a2)     ; DSTAT.SIR
        beq.s   .fail
        lea     DST,a0
        move.w  #WORDS-1,d0
.check: cmpi.l  #$42555354,(a0)+
        bne.s   .fail
        dbra    d0,.check
        move.w  #$2c4,$180(a6)
.hold:  bra.s   .hold
.fail:  move.w  #$f00,$180(a6)
        bra.s   .hold
