; SPDX-License-Identifier: GPL-3.0-or-later
; AGA Lisa: six-plane hold-and-modify in super-hires.
;
; OCS and ECS Denise decode HAM in low resolution only: a super-hires
; screen has at most two planes there. Lisa runs both HAM modes in every
; resolution, so six super-hires planes with HAMEN set are HAM6.
;
; Four 64-line bands. Every line is twenty 64-pixel groups, and every
; pixel of group g carries the same HAM6 code, with value g & 15:
;   band 1  modify red     a red ramp, 0 to 15, then 0 to 3 again
;   band 2  modify green   the same ramp in green
;   band 3  modify blue    the same ramp in blue
;   band 4  the three modify codes in turn (red, green, blue, red, ...):
;           each group changes one component and holds the two it did
;           not touch from the groups before it
; COLOR00 is black, so every line starts from a black hold colour. A
; decode that leaves HAM off in super-hires shows palette entries 16-63
; instead, which are all black here: a blank picture.
;
; Source: Commodore Lisa specification 391227 ("Both HAM modes are
; available in all resolutions (not just LORES as before)").
; Cross-checked against vAmiga 5.0b1 (tools/vamiga-ref.sh, A1200_2MB,
; VAMIGA_RGB=1): exact over the whole frame (tools/vamiga-aga-compare.py).
CUST    equ $dff000
PLANES  equ $40000               ; six planes, PSTRIDE apart
PSTRIDE equ $a000                ; 256 lines x 160 bytes
CLIST   equ $80000

        lea     CUST,a6
        move.w  #$7fff,$09a(a6)
        move.w  #$7fff,$09c(a6)
        move.w  #$7fff,$096(a6)

        ; Build the planes: d6 = plane, d7 = band, d4 = line, d5 = group.
        lea     PLANES,a0
        moveq   #0,d6
.plane: moveq   #0,d7
.band:  moveq   #64-1,d4
.line:  moveq   #0,d5
.group: moveq   #0,d0
        move.w  d7,d0            ; HAM control code for this group
        cmp.w   #3,d7
        bne.s   .fixed
        move.w  d5,d0            ; band 4 cycles red, green, blue
        divu    #3,d0
        swap    d0
.fixed: lea     codes(pc),a1
        move.b  0(a1,d0.w),d0    ; control in bits 5-4
        move.w  d5,d1
        and.w   #15,d1
        or.b    d1,d0            ; pixel index for the group
        btst    d6,d0
        sne     d1
        ext.w   d1
        ext.l   d1
        move.l  d1,(a0)+         ; 64 pixels of this plane
        move.l  d1,(a0)+
        addq.w  #1,d5
        cmp.w   #20,d5
        bne.s   .group
        dbra    d4,.line
        addq.w  #1,d7
        cmp.w   #4,d7
        bne.s   .band
        addq.w  #1,d6
        cmp.w   #6,d6
        bne.s   .plane

        ; Copper list: the six plane pointers, once a frame.
        lea     CLIST,a0
        move.l  #PLANES,d1
        move.w  #$00e0,d2
        moveq   #6-1,d6
.cptr:  move.w  d2,(a0)+
        swap    d1
        move.w  d1,(a0)+
        addq.w  #2,d2
        move.w  d2,(a0)+
        swap    d1
        move.w  d1,(a0)+
        addq.w  #2,d2
        add.l   #PSTRIDE,d1
        dbra    d6,.cptr
        move.l  #$fffffffe,(a0)+

        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        clr.w   $102(a6)
        clr.w   $104(a6)
        clr.w   $108(a6)
        clr.w   $10a(a6)
        move.w  #$6a41,$100(a6)  ; six planes, HAM, COLOR, SHRES, ECSENA
        move.w  #$0c00,$106(a6)
        move.w  #$0011,$10c(a6)
        move.w  #$0003,$1fc(a6)  ; 64-bit bitplane fetch
        clr.w   $180(a6)
        move.l  #CLIST,$080(a6)
        move.w  d0,$088(a6)
        move.w  #$8380,$096(a6)
.loop:  bra.s   .loop

codes:  dc.b    $20,$30,$10,$00  ; modify red, green, blue (HAM6 codes)
