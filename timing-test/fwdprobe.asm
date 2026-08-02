; 020 register-dependency / loop-alignment probe.
;
; Built to map the "result forwarding" surface after the main timing-test
; disk showed its RAW-dependent MOVE pair (row 29) running one clock per
; iteration faster than its independent pair (row 28). The real-A1200 run of
; THIS disk showed there is no forwarding at all: see "Measured on real
; hardware" below. The rows are kept as they were run, because together with
; the main disk's rows 4/7/28/29 they pin the effect that is really there.
;
; Measured on real hardware (A1200, 68EC020 at 14.19 MHz, KS 3.2.3,
; 2026-08-02, two screenshots in agreement; clocks = ticks * 20.006 / 8192):
;
;   every control row (0,2,4,..,18)  1003-1005  = 10.01 clk/iter
;   every RAW row     (1,3,5,..,19)  119D-119E  = 11.01 clk/iter
;   rows 20/21 (nop gap)             14EB       = 13.08
;   rows 22/23 (3-move chain)        14D0       = 13.01
;   row 24 (bare dbra)               099D       =  6.01
;   row 25 (one move)                0CD1       =  8.01
;
; The RAW rows are SLOWER, the opposite of the main disk's rows 28/29, so the
; dependency is not what is being measured. Cross-tabulating both disks closes
; the 2x2 -- independent/dbra%4==0 = 11.02 (main row 28), independent/2 =
; 10.01 (these controls), RAW/0 = 11.01 (these RAW rows), RAW/2 = 10.01 (main
; row 29): the register dependency has no effect whatsoever, and the whole
; difference tracks where the DBcc lands. Twenty-eight of the thirty measured
; loops across the two disks fit to within a tick,
;
;   clk/iter = 6 + 2 * (2-byte body instructions)
;                + 1 if the DBcc opcode word is longword-aligned
;
; i.e. a cached taken dbra costs 7 clocks at pc%4==0 and 6 at pc%4==2. The
; loop HEAD alignment varies freely inside each refill class, so it is the
; branch's own alignment that decides, not the target's; the presumed cause is
; the longword granularity of 020 instruction fetch (a dbra straddling two
; longwords has already had the second fetched when it retires). Every body
; instruction costs a flat 2 clocks regardless of shape -- .b/.w/.l MOVE,
; MOVEA either side, An-source MOVE, ADD, CMP and MOVEQ are identical -- so
; nothing here distinguishes a dependent operand from an independent one.
;
; The exceptions are rows 20/21, the NOP-gap couple: they measure 13.08 where
; the rule says 13.01, while rows 22/23 (three MOVEs, the same body count and
; the same dbra%4==0) sit exactly on 13.01. So a NOP is not quite a 2-clock
; body instruction on real silicon -- nearer 2.07 -- which is what a
; pipeline-synchronising instruction should look like. It is a 0.5% effect on
; one instruction, measured on one couple, so it is recorded here rather than
; modelled; Copperline emits the rule value 13.01 for both rows.
;
; Only DBcc loop branches were measured; Bcc/BSR are untested.
;
; Loaded by boot.asm to $30000. Takes over the machine (interrupts and DMA
; off), runs the row battery measured with the CIA-A timer A (E-clock =
; CPU clock / 10), renders each result as a two-digit decimal row ID plus
; 8 hex digits at a 7-scanline pitch, and streams the same values out the
; serial port. Caches are left exactly as the boot ROM set them (enabled on
; a 020+ AmigaOS boot), the same conditions the main disk's rows 28/29 were
; measured under.
;
; Every row is 8192 iterations of { pair ; dbra }. Rows come in
; control/RAW couples: the even row is the independent control, the odd row
; below it is identical except the second instruction sources the register
; the first just wrote. Only registers change between the two loops -- never
; instruction count, sizes, or addressing modes. One clock per iteration is
; about $19A E-ticks.
;
; Note the couples do NOT hold loop alignment constant: each control lands at
; dbra%4==2 and each RAW at dbra%4==0, which is why every couple differs by
; exactly the one alignment clock. That confound is what the main disk's rows
; 28/29 (the same two loops at the opposite alignments, giving the opposite
; ordering) resolve. A follow-up probe should pad each couple to hold the
; branch alignment fixed.
;
;   row 0  control  move.w d2,d0 + move.w d3,d1   (anchor: = main disk row 28)
;   row 1  RAW      move.w d2,d0 + move.w d0,d1   (anchor: = main disk row 29)
;   row 2  control  move.l d2,d0 + move.l d3,d1   (size .l)
;   row 3  RAW      move.l d2,d0 + move.l d0,d1
;   row 4  control  move.b d2,d0 + move.b d3,d1   (size .b)
;   row 5  RAW      move.b d2,d0 + move.b d0,d1
;   row 6  control  move.l d2,d0 + movea.l d3,a1  (MOVEA as consumer)
;   row 7  RAW      move.l d2,d0 + movea.l d0,a1
;   row 8  control  movea.l d2,a0 + move.l a4,d1  (MOVEA as producer)
;   row 9  RAW      movea.l d2,a0 + move.l a0,d1
;   row 10 control  move.l a2,d0 + move.l d3,d1   (An-source move as producer)
;   row 11 RAW      move.l a2,d0 + move.l d0,d1
;   row 12 control  add.w d2,d0 + move.w d3,d1    (ALU op as producer)
;   row 13 RAW      add.w d2,d0 + move.w d0,d1
;   row 14 control  move.w d2,d0 + add.w d3,d1    (ALU op as consumer)
;   row 15 RAW      move.w d2,d0 + add.w d0,d1
;   row 16 control  move.w d2,d0 + cmp.w d3,d1    (CCR-only consumer)
;   row 17 RAW      move.w d2,d0 + cmp.w d0,d1
;   row 18 control  moveq #5,d0 + move.w d3,d1    (MOVEQ as producer)
;   row 19 RAW      moveq #5,d0 + move.w d0,d1
;   row 20 control  move.w d2,d0 + nop + move.w d3,d1  (one-instruction gap)
;   row 21 RAW      move.w d2,d0 + nop + move.w d0,d1
;   row 22 control  move.w d2,d0 + move.w d3,d1 + move.w d2,d4  (3-move chain,
;   row 23 RAW      move.w d2,d0 + move.w d0,d1 + move.w d1,d4   both links RAW)
;   row 24 bare dbra loop                         (anchor: = main disk row 7)
;   row 25 single move.w d2,d0                    (anchor: = main disk row 4)
;
; Rows 0/1, 24 and 25 tie this disk to the main timing-test disk: on the same
; machine they must reproduce that disk's rows 28, 29, 7 and 4.
;
; Scratch chip-RAM addresses: $40000 screen (1 plane 320x256), $48000 results.
; Position-independent (PC-relative + fixed scratch addresses).

CUST    equ     $dff000
ITERS   equ     $2000           ; 8192 iterations per row
NROWS   equ     26
SCREEN  equ     $40000
RESULTS equ     $48000

;----------------------------------------------------- entry (a6=sys at load)
boot:
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)  ; INTENA: disable all interrupts
        move.w  #$7fff,$9c(a6)  ; INTREQ: clear all pending
        move.w  #$7fff,$96(a6)  ; DMACON: disable all DMA
        move.w  #$0f00,$180(a6) ; "alive" border colour (red) until display is up

        ; clear the screen bitplane
        lea     SCREEN,a0
        move.w  #(40*256/4)-1,d0
.clrs   clr.l   (a0)+
        dbra    d0,.clrs

        ; deterministic register file for the measured pairs: d2/d3 and a2/a4
        ; are the (even, never-dereferenced) sources, d0/d1/d4 and a0/a1 the
        ; destinations. a3 is the result write pointer, never a source.
        move.l  #$00040000,d2
        move.l  #$00050000,d3
        moveq   #0,d0
        moveq   #0,d1
        moveq   #0,d4
        movea.l d2,a2
        movea.l d3,a4           ; a4 = independent An source for row 8
                                ; (a3 is the result write pointer below)

        lea     RESULTS,a3      ; result write pointer

        ; row 0: control .w  (= main disk row 28)
        bsr     tstart
        move.w  #ITERS-1,d6
.t00    move.w  d2,d0
        move.w  d3,d1
        dbra    d6,.t00
        bsr     tread
        move.l  d0,(a3)+

        ; row 1: RAW .w  (= main disk row 29)
        bsr     tstart
        move.w  #ITERS-1,d6
.t01    move.w  d2,d0
        move.w  d0,d1
        dbra    d6,.t01
        bsr     tread
        move.l  d0,(a3)+

        ; row 2: control .l
        bsr     tstart
        move.w  #ITERS-1,d6
.t02    move.l  d2,d0
        move.l  d3,d1
        dbra    d6,.t02
        bsr     tread
        move.l  d0,(a3)+

        ; row 3: RAW .l
        bsr     tstart
        move.w  #ITERS-1,d6
.t03    move.l  d2,d0
        move.l  d0,d1
        dbra    d6,.t03
        bsr     tread
        move.l  d0,(a3)+

        ; row 4: control .b
        bsr     tstart
        move.w  #ITERS-1,d6
.t04    move.b  d2,d0
        move.b  d3,d1
        dbra    d6,.t04
        bsr     tread
        move.l  d0,(a3)+

        ; row 5: RAW .b
        bsr     tstart
        move.w  #ITERS-1,d6
.t05    move.b  d2,d0
        move.b  d0,d1
        dbra    d6,.t05
        bsr     tread
        move.l  d0,(a3)+

        ; row 6: control, MOVEA consumer
        bsr     tstart
        move.w  #ITERS-1,d6
.t06    move.l  d2,d0
        movea.l d3,a1
        dbra    d6,.t06
        bsr     tread
        move.l  d0,(a3)+

        ; row 7: RAW into MOVEA (does the latch feed an An write?)
        bsr     tstart
        move.w  #ITERS-1,d6
.t07    move.l  d2,d0
        movea.l d0,a1
        dbra    d6,.t07
        bsr     tread
        move.l  d0,(a3)+

        ; row 8: control, MOVEA producer (a4 = independent An source)
        bsr     tstart
        move.w  #ITERS-1,d6
.t08    movea.l d2,a0
        move.l  a4,d1
        dbra    d6,.t08
        bsr     tread
        move.l  d0,(a3)+

        ; row 9: RAW from MOVEA (does an An write arm the latch?)
        bsr     tstart
        move.w  #ITERS-1,d6
.t09    movea.l d2,a0
        move.l  a0,d1
        dbra    d6,.t09
        bsr     tread
        move.l  d0,(a3)+

        ; row 10: control, An-source MOVE producer
        bsr     tstart
        move.w  #ITERS-1,d6
.t10    move.l  a2,d0
        move.l  d3,d1
        dbra    d6,.t10
        bsr     tread
        move.l  d0,(a3)+

        ; row 11: RAW after an An-source MOVE (the Dn write should latch)
        bsr     tstart
        move.w  #ITERS-1,d6
.t11    move.l  a2,d0
        move.l  d0,d1
        dbra    d6,.t11
        bsr     tread
        move.l  d0,(a3)+

        ; row 12: control, ALU producer
        bsr     tstart
        move.w  #ITERS-1,d6
.t12    add.w   d2,d0
        move.w  d3,d1
        dbra    d6,.t12
        bsr     tread
        move.l  d0,(a3)+

        ; row 13: RAW after an ALU write (does ADD arm the latch?)
        bsr     tstart
        move.w  #ITERS-1,d6
.t13    add.w   d2,d0
        move.w  d0,d1
        dbra    d6,.t13
        bsr     tread
        move.l  d0,(a3)+

        ; row 14: control, ALU consumer
        bsr     tstart
        move.w  #ITERS-1,d6
.t14    move.w  d2,d0
        add.w   d3,d1
        dbra    d6,.t14
        bsr     tread
        move.l  d0,(a3)+

        ; row 15: RAW into an ALU op (does ADD read the latch?)
        bsr     tstart
        move.w  #ITERS-1,d6
.t15    move.w  d2,d0
        add.w   d0,d1
        dbra    d6,.t15
        bsr     tread
        move.l  d0,(a3)+

        ; row 16: control, CCR-only consumer
        bsr     tstart
        move.w  #ITERS-1,d6
.t16    move.w  d2,d0
        cmp.w   d3,d1
        dbra    d6,.t16
        bsr     tread
        move.l  d0,(a3)+

        ; row 17: RAW into CMP (compare reads the latch, writes only CCR)
        bsr     tstart
        move.w  #ITERS-1,d6
.t17    move.w  d2,d0
        cmp.w   d0,d1
        dbra    d6,.t17
        bsr     tread
        move.l  d0,(a3)+

        ; row 18: control, MOVEQ producer
        bsr     tstart
        move.w  #ITERS-1,d6
.t18    moveq   #5,d0
        move.w  d3,d1
        dbra    d6,.t18
        bsr     tread
        move.l  d0,(a3)+

        ; row 19: RAW after MOVEQ (does MOVEQ arm the latch?)
        bsr     tstart
        move.w  #ITERS-1,d6
.t19    moveq   #5,d0
        move.w  d0,d1
        dbra    d6,.t19
        bsr     tread
        move.l  d0,(a3)+

        ; row 20: control with a one-instruction gap
        bsr     tstart
        move.w  #ITERS-1,d6
.t20    move.w  d2,d0
        nop
        move.w  d3,d1
        dbra    d6,.t20
        bsr     tread
        move.l  d0,(a3)+

        ; row 21: RAW across a one-instruction gap (latch lifetime)
        bsr     tstart
        move.w  #ITERS-1,d6
.t21    move.w  d2,d0
        nop
        move.w  d0,d1
        dbra    d6,.t21
        bsr     tread
        move.l  d0,(a3)+

        ; row 22: control 3-move chain
        bsr     tstart
        move.w  #ITERS-1,d6
.t22    move.w  d2,d0
        move.w  d3,d1
        move.w  d2,d4
        dbra    d6,.t22
        bsr     tread
        move.l  d0,(a3)+

        ; row 23: RAW 3-move chain (both links dependent; reads identical to
        ; row 22 on real hardware)
        bsr     tstart
        move.w  #ITERS-1,d6
.t23    move.w  d2,d0
        move.w  d0,d1
        move.w  d1,d4
        dbra    d6,.t23
        bsr     tread
        move.l  d0,(a3)+

        ; row 24: bare dbra loop (anchor: = main disk row 7)
        bsr     tstart
        move.w  #ITERS-1,d6
.t24    dbra    d6,.t24
        bsr     tread
        move.l  d0,(a3)+

        ; row 25: single register move (anchor: = main disk row 4)
        bsr     tstart
        move.w  #ITERS-1,d6
.t25    move.w  d2,d0
        dbra    d6,.t25
        bsr     tread
        move.l  d0,(a3)+

        move.w  #$0ff0,$180(a6) ; phase marker: all rows done (yellow)

        ;------------------------------------------------ render + show
        bsr     render

        ; Stream the results out the serial port as ASCII hex (one 8-digit
        ; value per line), same format as the main disk.
        move.w  #$0170,$032(a6) ; SERPER ~9600 baud
        lea     RESULTS,a2
        moveq   #NROWS-1,d4
.sl     move.l  (a2)+,d3
        moveq   #8-1,d6
.sh     rol.l   #4,d3
        move.l  d3,d0
        and.w   #$f,d0
        add.w   #'0',d0
        cmp.w   #'9',d0
        ble     .sok
        addq.w  #7,d0           ; 'A'..'F'
.sok    bsr     sendb
        dbra    d6,.sh
        moveq   #13,d0          ; CR
        bsr     sendb
        moveq   #10,d0          ; LF
        bsr     sendb
        dbra    d4,.sl

        ; Bring up a single lores bitplane directly from the CPU (no copper).
        move.w  #$1000,$100(a6) ; BPLCON0: 1 bitplane, lores
        move.w  #$0000,$102(a6) ; BPLCON1
        move.w  #$0000,$104(a6) ; BPLCON2
        move.w  #$0000,$108(a6) ; BPL1MOD
        move.w  #$0038,$092(a6) ; DDFSTRT
        move.w  #$00d0,$094(a6) ; DDFSTOP
        move.w  #$2c81,$08e(a6) ; DIWSTRT
        move.w  #$2cc1,$090(a6) ; DIWSTOP
        move.w  #$0000,$180(a6) ; COLOR00 black
        move.w  #$0fff,$182(a6) ; COLOR01 white
        move.w  #$8300,$096(a6) ; DMAEN | BPLEN
.show:
        bsr     syncframe
        move.l  #SCREEN,d0
        move.w  d0,$0e2(a6)     ; BPL1PTL
        swap    d0
        move.w  d0,$0e0(a6)     ; BPL1PTH
        bra     .show

;------------------------------------------------ send one char (d0.b) on serial
sendb:
.tbe    move.w  $018(a6),d1     ; SERDATR
        btst    #13,d1
        beq     .tbe
        and.w   #$ff,d0
        or.w    #$100,d0
        move.w  d0,$030(a6)     ; SERDAT
        rts

;------------------------------------------------ read the live beam vpos -> d0
getvpos:
        move.w  $004(a6),d0     ; VPOSR
        and.w   #1,d0           ; V8
        lsl.w   #8,d0           ; -> bit 8
        move.w  $006(a6),d1     ; VHPOSR
        lsr.w   #8,d1           ; V7..V0
        or.w    d1,d0           ; full vpos
        rts

;------------------------------------------------ sync to the next frame start
syncframe:
.hi     bsr     getvpos
        cmp.w   #280,d0
        blo     .hi             ; wait until near the bottom of the frame
.wrap   bsr     getvpos
        cmp.w   #280,d0
        bhs     .wrap           ; wait until vpos wraps back to the top
        rts

;------------------------------------------------ CIA-A timer A: start countdown
tstart:
        move.b  #$ff,$bfe401    ; TA low latch
        move.b  #$ff,$bfe501    ; TA high latch
        move.b  #$19,$bfee01    ; CRA: load + one-shot + start
        rts

;------------------------------------------------ read elapsed ticks -> d0.w
tread:
        move.b  #$08,$bfee01    ; CRA: stop (one-shot)
        moveq   #0,d0
        move.b  $bfe501,d0      ; TA high
        lsl.w   #8,d0
        move.b  $bfe401,d0      ; TA low
        not.w   d0              ; elapsed = $ffff - remaining
        rts

;------------------------------------------------ render all NROWS results
; One row per test: two-digit decimal row ID, a blank column, then the value
; as 8 hex digits, at a 7-scanline pitch inside the CRT-safe area.
render:
        lea     SCREEN,a1       ; clear the bitplane
        move.w  #40*256/4-1,d0
.rc:
        clr.l   (a1)+
        dbra    d0,.rc
        lea     RESULTS,a2
        moveq   #0,d4           ; row index
.rr:
        move.l  (a2)+,d3        ; value
        move.w  d4,d0
        mulu    #280,d0         ; 7 scanlines * 40 bytes per row
        lea     SCREEN,a5
        adda.l  d0,a5           ; row top in bitplane
        move.w  d4,d0           ; two-digit decimal row ID in columns 0-1
        ext.l   d0
        divu    #10,d0          ; low word = tens, high word = ones
        moveq   #0,d2           ; column (byte) index
        bsr     .glyph
        swap    d0              ; ones digit
        bsr     .glyph
        addq.w  #1,d2           ; blank column between row ID and value
        moveq   #7,d6           ; 8 hex digits
.rd:
        rol.l   #4,d3           ; next nibble (high first) into low 4 bits
        move.l  d3,d0
        bsr     .glyph
        dbra    d6,.rd
        addq.w  #1,d4
        cmp.w   #NROWS,d4
        bne     .rr
        rts

; Draw the glyph for digit d0.w (low 4 bits) at column d2 of the row at a5;
; advances d2. Draws 7 glyph lines -- the font's 8th line is blank, so the
; 7-scanline row pitch loses nothing.
.glyph:
        and.w   #$f,d0
        lsl.w   #3,d0           ; * 8 bytes per glyph
        lea     font(pc),a4
        adda.w  d0,a4
        move.l  a5,a1
        adda.w  d2,a1           ; + column byte
        moveq   #6,d5           ; 7 glyph lines
.rg:
        move.b  (a4)+,(a1)
        adda.w  #40,a1
        dbra    d5,.rg
        addq.w  #1,d2
        rts

;------------------------------------------------ 8x8 hex font, glyphs 0..F
font:
        dc.b $70,$88,$98,$a8,$c8,$88,$70,$00    ; 0
        dc.b $20,$60,$20,$20,$20,$20,$70,$00    ; 1
        dc.b $70,$88,$08,$10,$20,$40,$f8,$00    ; 2
        dc.b $70,$88,$08,$30,$08,$88,$70,$00    ; 3
        dc.b $10,$30,$50,$90,$f8,$10,$10,$00    ; 4
        dc.b $f8,$80,$f0,$08,$08,$88,$70,$00    ; 5
        dc.b $30,$40,$80,$f0,$88,$88,$70,$00    ; 6
        dc.b $f8,$08,$10,$20,$40,$40,$40,$00    ; 7
        dc.b $70,$88,$88,$70,$88,$88,$70,$00    ; 8
        dc.b $70,$88,$88,$78,$08,$10,$60,$00    ; 9
        dc.b $70,$88,$88,$f8,$88,$88,$88,$00    ; A
        dc.b $f0,$88,$88,$f0,$88,$88,$f0,$00    ; B
        dc.b $70,$88,$80,$80,$80,$88,$70,$00    ; C
        dc.b $e0,$90,$88,$88,$88,$90,$e0,$00    ; D
        dc.b $f8,$80,$80,$f0,$80,$80,$f8,$00    ; E
        dc.b $f8,$80,$80,$f0,$80,$80,$80,$00    ; F
