; 68020 bit-field instruction cost probe (BFSET/BFTST/BFEXTU/BFINS).
;
; SANITY Roots II AGA's "DIE" dissolve (issue #371) plots two BFSET
; (a1){d2:1} bits per pixel from a per-frame software interrupt; the worker
; must finish inside one PAL frame or the effect halves to 25 Hz. That
; hinges on what a memory-form bit-field RMW really costs on the 020: the
; MC68020UM 8.2.14 table bills a field within four bytes as ONE operand
; read (plus one write for the modify forms) on top of 16 clocks cache
; case, and a five-byte span one operand cycle more, but no real-hardware
; measurement of the bit-field class exists in any emulator's calibration
; that we know of. These rows isolate: the register-form internal cost,
; the memory RMW cost by span (1 / 2 / 4 / 5 bytes), the dynamic-offset
; form, the read-only forms, and the demo's exact nine-instruction inner
; loop, all with the DBcc alignment controlled (a taken DBcc at pc%4==0
; costs one clock more than at %4==2, see fwdprobe).
;
; Rows (8192 iterations; alignments listed as assembled, verify with
; 51CE scans of the binary):
;
;   row  0  move.w d2,d0 + dbra        no access, %4=0   anchor = 0E6A
;   row  1  move.w d2,d0 + dbra        no access, %4=2   anchor = 0CD0
;   row  2  bfset (a0){0:1}            span 1,    %4=2
;   row  3  bfset (a0){0:1}            span 1,    %4=0
;   row  4  bfset d1{0:1}              register,  %4=2
;   row  5  bfset (a0){4:8}            span 2,    %4=2
;   row  6  bfset (a0){0:32}           span 4,    %4=2
;   row  7  bfset (a0){7:32}           span 5,    %4=2
;   row  8  bfset (a0){d5:1}, d5=3     dynamic,   %4=2
;   row  9  bftst (a0){0:1}            read-only, %4=2
;   row 10  bfextu (a0){4:16},d0       2-byte rd, %4=2
;   row 11  bfins d1,(a0){0:1}         insert,    %4=2
;   row 12  Roots II plot loop verbatim            %4=0  (the demo's)
;   row 13  Roots II plot loop verbatim            %4=2
;
; Row 12/13 body (the issue #371 worker; delta stream zeroed so every
; BFSET lands on the same byte):
;   move.l (a0),d1 / add.l (a2)+,d1 / move.l d1,(a0)+ / move.w d1,d2
;   bfset (a1){d2:1} / swap d1 / move.w d1,d2 / bfset (a1){d2:1} / dbra
;
; The bit-field opcodes are hand-encoded (dc.w) so the disk keeps
; assembling with vasm -m68000 like every other probe.
;
; No real-A1200 column exists yet. Emulator columns 2026-08-03 (E-clock
; ticks; clk/iter = ticks * 20.006 / 8192), after the m68k crate's
; bit-field access-width and 020 timing fix:
;
;              Copperline           FS-UAE (WinUAE core)
;   row  0     0E6C  9.02 clk       1006  10.02 clk   (anchor)
;   row  1     0CD0  8.01           1004  10.01       (anchor)
;   row  2     19BC 16.09           204C  20.19
;   row  3     204C 20.19           204D  20.19
;   row  4     1CD0 18.02           1337  12.01
;   row  5     3A22 36.35           204C  20.19
;   row  6     7A38 76.42           204D  20.19
;   row  7     9A94 96.66           3A22  36.35
;   row  8     19BC 16.09           204C  20.19
;   row  9     19BB 16.09           1337  12.01
;   row 10     4096 40.38           204C  20.19
;   row 11     19BB 16.09           204C  20.19
;   row 12     66FB 64.39           6D42  68.31
;   row 13     60E1 60.57           6D43  68.31
;
; The columns already disagree structurally: FS-UAE bills spans 1, 2 and
; 4 identically (rows 2/5/6 - the UM's one-long-operand model) where
; Copperline's byte-granular accesses grow with the span, and FS-UAE's
; demo-loop rows (12/13) are SLOWER than Copperline's yet FS-UAE meets
; the demo's frame deadline. Only a real-A1200 column can arbitrate.
;
; Loaded by boot.asm to $30000. Same CIA-A timer A harness, renderer and
; serial output as rdprobe: a two-digit decimal row ID then the value as
; 8 hex digits, at a 7-scanline pitch inside the CRT-safe area.
; Scratch: $40000 screen, $48000 results, $60000 chip target, $64000
; tracer array, $70000 delta stream (zeroed).

CUST    equ     $dff000
ITERS   equ     $2000
NROWS   equ     14
SCREEN  equ     $40000
RESULTS equ     $48000
CHIPT   equ     $60000
TRACER  equ     $64000
DELTAS  equ     $70000
boot:
        lea     CUST,a6
        move.w  #$7fff,$9a(a6)
        move.w  #$7fff,$9c(a6)
        move.w  #$7fff,$96(a6)
        move.w  #$0f00,$180(a6)
        lea     SCREEN,a0
        move.w  #(40*256/4)-1,d0
.clrs   clr.l   (a0)+
        dbra    d0,.clrs
        ; zero the row-12/13 tracer array and delta stream (8192 longs each)
        lea     TRACER,a0
        move.w  #ITERS-1,d0
.clrt   clr.l   (a0)+
        dbra    d0,.clrt
        lea     DELTAS,a0
        move.w  #ITERS-1,d0
.clrd   clr.l   (a0)+
        dbra    d0,.clrd
        move.l  #$00040000,d2
        move.l  #$00050000,d3
        moveq   #3,d5
        lea     RESULTS,a3

        ; row 0: register move, dbra longword aligned (anchor = 9 clk)
        cnop    0,4
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t00    move.w  d2,d0
        dbra    d6,.t00
        bsr     tread
        move.l  d0,(a3)+

        ; row 1: register move, dbra at %4==2 (anchor = 8 clk)
        cnop    0,4
        bsr     tstart
        move.w  #ITERS-1,d6
.t01    move.w  d2,d0
        dbra    d6,.t01
        bsr     tread
        move.l  d0,(a3)+

        ; row 2: bfset (a0){0:1}, span 1, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t02    dc.w    $EED0,$0001     ; bfset (a0){0:1}
        dbra    d6,.t02
        bsr     tread
        move.l  d0,(a3)+

        ; row 3: bfset (a0){0:1}, span 1, dbra longword aligned
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t03    dc.w    $EED0,$0001     ; bfset (a0){0:1}
        dbra    d6,.t03
        bsr     tread
        move.l  d0,(a3)+

        ; row 4: bfset d1{0:1}, register form, dbra at %4==2
        cnop    0,4
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
.t04    dc.w    $EEC1,$0001     ; bfset d1{0:1}
        dbra    d6,.t04
        bsr     tread
        move.l  d0,(a3)+

        ; row 5: bfset (a0){4:8}, two-byte span, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t05    dc.w    $EED0,$0108     ; bfset (a0){4:8}
        dbra    d6,.t05
        bsr     tread
        move.l  d0,(a3)+

        ; row 6: bfset (a0){0:32}, four-byte span, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t06    dc.w    $EED0,$0000     ; bfset (a0){0:32}
        dbra    d6,.t06
        bsr     tread
        move.l  d0,(a3)+

        ; row 7: bfset (a0){7:32}, five-byte span, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t07    dc.w    $EED0,$01C0     ; bfset (a0){7:32}
        dbra    d6,.t07
        bsr     tread
        move.l  d0,(a3)+

        ; row 8: bfset (a0){d5:1}, dynamic offset (d5=3), dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t08    dc.w    $EED0,$0941     ; bfset (a0){d5:1}
        dbra    d6,.t08
        bsr     tread
        move.l  d0,(a3)+

        ; row 9: bftst (a0){0:1}, read-only, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t09    dc.w    $E8D0,$0001     ; bftst (a0){0:1}
        dbra    d6,.t09
        bsr     tread
        move.l  d0,(a3)+

        ; row 10: bfextu (a0){4:16},d0, two-byte read, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t10    dc.w    $E9D0,$0110     ; bfextu (a0){4:16},d0
        dbra    d6,.t10
        bsr     tread
        move.l  d0,(a3)+

        ; row 11: bfins d1,(a0){0:1}, dbra at %4==2
        cnop    0,4
        lea     CHIPT,a0
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t11    dc.w    $EFD0,$1001     ; bfins d1,(a0){0:1}
        dbra    d6,.t11
        bsr     tread
        move.l  d0,(a3)+

        ; row 12: the Roots II plot loop, dbra longword aligned (as the
        ; demo assembles it: its dbra sits at $3C090)
        cnop    0,4
        lea     TRACER,a0
        lea     CHIPT,a1
        lea     DELTAS,a2
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
        nop
.t12    move.l  (a0),d1
        add.l   (a2)+,d1
        move.l  d1,(a0)+
        move.w  d1,d2
        dc.w    $EED1,$0881     ; bfset (a1){d2:1}
        swap    d1
        move.w  d1,d2
        dc.w    $EED1,$0881     ; bfset (a1){d2:1}
        dbra    d6,.t12
        bsr     tread
        move.l  d0,(a3)+

        ; row 13: the same loop, dbra at %4==2
        cnop    0,4
        lea     TRACER,a0
        lea     CHIPT,a1
        lea     DELTAS,a2
        bsr     tstart
        move.w  #ITERS-1,d6
        nop
        nop
.t13    move.l  (a0),d1
        add.l   (a2)+,d1
        move.l  d1,(a0)+
        move.w  d1,d2
        dc.w    $EED1,$0881     ; bfset (a1){d2:1}
        swap    d1
        move.w  d1,d2
        dc.w    $EED1,$0881     ; bfset (a1){d2:1}
        dbra    d6,.t13
        bsr     tread
        move.l  d0,(a3)+

        move.w  #$0ff0,$180(a6)
        bsr     render
        move.w  #$0170,$032(a6)
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
        addq.w  #7,d0
.sok    bsr     sendb
        dbra    d6,.sh
        moveq   #13,d0
        bsr     sendb
        moveq   #10,d0
        bsr     sendb
        dbra    d4,.sl
        move.w  #$1000,$100(a6)
        move.w  #$0000,$102(a6)
        move.w  #$0000,$104(a6)
        move.w  #$0000,$108(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0000,$180(a6)
        move.w  #$0fff,$182(a6)
        move.w  #$8300,$096(a6)
.show:  bsr     syncframe
        move.l  #SCREEN,d0
        move.w  d0,$0e2(a6)
        swap    d0
        move.w  d0,$0e0(a6)
        bra     .show
sendb:
.tbe    move.w  $018(a6),d1
        btst    #13,d1
        beq     .tbe
        and.w   #$ff,d0
        or.w    #$100,d0
        move.w  d0,$030(a6)
        rts
getvpos:
        move.w  $004(a6),d0
        and.w   #1,d0
        lsl.w   #8,d0
        move.w  $006(a6),d1
        lsr.w   #8,d1
        or.w    d1,d0
        rts
syncframe:
.hi     bsr     getvpos
        cmp.w   #280,d0
        blo     .hi
.wrap   bsr     getvpos
        cmp.w   #280,d0
        bhs     .wrap
        rts
tstart:
        move.b  #$ff,$bfe401
        move.b  #$ff,$bfe501
        move.b  #$19,$bfee01
        rts
tread:
        move.b  #$08,$bfee01
        moveq   #0,d0
        move.b  $bfe501,d0
        lsl.w   #8,d0
        move.b  $bfe401,d0
        not.w   d0
        rts
render:
        lea     SCREEN,a1
        move.w  #40*256/4-1,d0
.rc:    clr.l   (a1)+
        dbra    d0,.rc
        lea     RESULTS,a2
        moveq   #0,d4
.rr:    move.l  (a2)+,d3
        move.w  d4,d0
        mulu    #280,d0
        lea     SCREEN,a5
        adda.l  d0,a5
        move.w  d4,d0
        ext.l   d0
        divu    #10,d0
        moveq   #0,d2
        bsr     .glyph
        swap    d0
        bsr     .glyph
        addq.w  #1,d2
        moveq   #7,d6
.rd:    rol.l   #4,d3
        move.l  d3,d0
        bsr     .glyph
        dbra    d6,.rd
        addq.w  #1,d4
        cmp.w   #NROWS,d4
        bne     .rr
        rts
.glyph:
        and.w   #$f,d0
        lsl.w   #3,d0
        lea     font(pc),a4
        adda.w  d0,a4
        move.l  a5,a1
        adda.w  d2,a1
        moveq   #6,d5
.rg:    move.b  (a4)+,(a1)
        adda.w  #40,a1
        dbra    d5,.rg
        addq.w  #1,d2
        rts
font:
        dc.b $70,$88,$98,$a8,$c8,$88,$70,$00
        dc.b $20,$60,$20,$20,$20,$20,$70,$00
        dc.b $70,$88,$08,$10,$20,$40,$f8,$00
        dc.b $70,$88,$08,$30,$08,$88,$70,$00
        dc.b $10,$30,$50,$90,$f8,$10,$10,$00
        dc.b $f8,$80,$f0,$08,$08,$88,$70,$00
        dc.b $30,$40,$80,$f0,$88,$88,$70,$00
        dc.b $f8,$08,$10,$20,$40,$40,$40,$00
        dc.b $70,$88,$88,$70,$88,$88,$70,$00
        dc.b $70,$88,$88,$78,$08,$10,$60,$00
        dc.b $70,$88,$88,$f8,$88,$88,$88,$00
        dc.b $f0,$88,$88,$f0,$88,$88,$f0,$00
        dc.b $70,$88,$80,$80,$80,$88,$70,$00
        dc.b $e0,$90,$88,$88,$88,$90,$e0,$00
        dc.b $f8,$80,$80,$f0,$80,$80,$f8,$00
        dc.b $f8,$80,$80,$f0,$80,$80,$80,$00
