; Asset-free 68000/CIA/Paula regression fixture for nested level-2/level-6
; CIA timer interrupts during a register-sourced sample loop.
;
; This is an independently written reproducer, not a copy of Voyage or its
; loader. The two interrupt handlers deliberately save and restore equal-size
; MOVEM lists with shifted destination registers, so the frames are balanced
; while live D-register values are permuted. Both handlers acknowledge their
; CIA ICR and Paula request. The main loop writes six register bytes 100 times.

        org     $00030000

CUSTOM          equ     $00dff000
CIAA_TALO       equ     $00bfe401
CIAA_TAHI       equ     $00bfe501
CIAA_ICR        equ     $00bfed01
CIAA_CRA        equ     $00bfee01
CIAB_TALO       equ     $00bfd400
CIAB_TAHI       equ     $00bfd500
CIAB_ICR        equ     $00bfdd00
CIAB_CRA        equ     $00bfde00

SAMPLE          equ     $00031000
L2_COUNT        equ     $00032000
L6_COUNT        equ     $00032002
DONE_WORD       equ     $00032004
DONE_MAGIC      equ     $43494152          ; "CIAR"

program_start:
        move.w  #$2700,sr                  ; configure while interrupts masked
        move.l  #level2_handler,$0068.w    ; L2 autovector
        move.l  #level6_handler,$0078.w    ; L6 autovector
        clr.w   L2_COUNT.l
        clr.w   L6_COUNT.l
        clr.l   DONE_WORD.l

        ; Fill the entire buffer first so incomplete loops are visible.
        lea     SAMPLE,a0
        move.w  #599,d0
.clear_sample:
        move.b  #$a5,(a0)+
        dbra    d0,.clear_sample

        ; Six distinct initial values are sampled as bytes. Handlers leave D7
        ; alone so it remains the loop counter while D0-D6 may be permuted.
        move.l  #$10203040,d0
        move.l  #$51617181,d1
        move.l  #$92a2b2c2,d2
        move.l  #$d3e3f304,d3
        move.l  #$25354555,d4
        move.l  #$66768696,d5
        move.l  #$a7b7c7d7,d6
        lea     SAMPLE+600,a1

        ; Choose 0..15 NOPs between A and B timer starts. The fixed setup and
        ; dispatch cost is identical for every phase; only the relative timer
        ; start phase changes. Test code patches the guarded phase word.
        lea     .phase_pad_end(pc),a2
        move.w  phase_pad_count(pc),d7
        andi.w  #15,d7
        add.w   d7,d7
        suba.w  d7,a2

        ; Start from a known disabled/acknowledged state before configuring
        ; CIA-A timer A and CIA-B timer A and their Paula interrupt routes.
        move.w  #$7fff,CUSTOM+$09a          ; clear all Paula interrupt masks
        move.b  #$00,CIAA_CRA
        move.b  #$00,CIAB_CRA
        move.b  #$7f,CIAA_ICR               ; clear CIA-A interrupt mask bits
        move.b  #$7f,CIAB_ICR               ; clear CIA-B interrupt mask bits
        move.b  CIAA_ICR,d7                 ; clear any latched ICR source flags
        move.b  CIAB_ICR,d7
        move.w  #$2000,CUSTOM+$09c          ; clear EXTER after reading CIA ICRs
        move.w  #$0008,CUSTOM+$09c          ; clear PORTS after reading CIA ICRs

        ; Match the measured order: start short CIA-B ($01ff) first, then
        ; long CIA-A ($024f) after the selectable relative phase interval.
        move.b  #$ff,CIAB_TALO
        move.b  #$01,CIAB_TAHI
        move.b  #$4f,CIAA_TALO
        move.b  #$02,CIAA_TAHI
        move.b  #$81,CIAB_ICR
        move.b  #$81,CIAA_ICR
        move.w  #$e008,CUSTOM+$09a          ; SET master + PORTS + EXTER
        ori.b   #$11,CIAB_CRA                ; START | FORCE LOAD CIA-B timer A
        jmp     (a2)
.phase_pad:
        rept    16
        nop
        endr
.phase_pad_end:
        ori.b   #$11,CIAA_CRA                ; START | FORCE LOAD CIA-A timer A
        moveq   #99,d7
        stop    #$2000                     ; wait for short CIA-B's first L6 wake

        ; 100 * six byte predecrement stores = 600 bytes; the write ordering
        ; and six distinct source registers make asynchronous register shifts
        ; observable after the repeated CIA interrupts.
sample_loop:
        move.b  d0,-(a1)
        move.b  d1,-(a1)
        move.b  d2,-(a1)
        move.b  d3,-(a1)
        move.b  d4,-(a1)
        move.b  d5,-(a1)
        dbra    d7,sample_loop

        move.w  #$7fff,CUSTOM+$09a          ; disable all Paula interrupt masks
        move.b  #$00,CIAA_CRA
        move.b  #$00,CIAB_CRA
        move.l  #DONE_MAGIC,DONE_WORD.l
.done:
        stop    #$2700
        bra.s   .done

; L2 maps saved D0-D3 to D0/D4-D6 on restore. Same five longwords are popped,
; so SP is restored exactly while the live D-register assignment changes.
level2_handler:
        movem.l d0-d3/a0,-(sp)
        move.b  CIAA_ICR,d0                 ; acknowledge CIA-A timer A
        move.w  #$0008,CUSTOM+$09c          ; clear Paula PORTS request
        addq.w  #1,L2_COUNT.l
        movem.l (sp)+,d0/d4-d6/a0
        rte

; L6 uses a different equal-length mapping: saved D1-D4 become D1/D3-D5.
; Higher priority allows this handler to nest inside the L2 handler.
level6_handler:
        movem.l d1-d4/a0,-(sp)
        move.b  CIAB_ICR,d1                 ; acknowledge CIA-B timer A
        move.w  #$2000,CUSTOM+$09c          ; clear Paula EXTER request
        addq.w  #1,L6_COUNT.l
        movem.l (sp)+,d1/d3-d5/a0
        rte

        even
phase_guard_before:
        dc.l    $c1a1c0de
phase_pad_count:
        dc.w    0
phase_guard_after:
        dc.l    $deadfeed
