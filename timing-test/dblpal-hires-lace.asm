; Programmable 31 kHz SHRES + interlace display and DMA-sprite probe.
;
; This is the custom-chip constellation used by the AmigaOS DblPAL
; "High Res Laced" mode:
;
;   HTOTAL=$081 (130 cck), VTOTAL=$23D (574 lines)
;   BEAMCON0=$1B88 (programmable beam/sync/blanking, LOLDIS)
;   BPLCON0=$2245 (2 planes, SHRES, LACE, ECSENA)
;   FMODE=$8003 (SSCAN2 plus 64-bit bitplane fetch)
;
; The two bitplanes draw a dense checker/ruler pattern across a 640-sample
; row.  Sprite 0 is a red 16-sample bar at HSTART=357, the exact comparator
; position captured from the missing Workbench pointer.  Sprite 1 is a green
; control bar at HSTART=128.  Both cover beam lines 42..73.
;
; On the affected Copperline build only the green control sprite appears.  The
; red sprite is captured correctly by Agnus, but SSCAN2's HSTART high-bit mask
; is missing: Copperline maps $165 literally instead of comparing it as $065,
; so the sprite lands beyond the emulated DIW gate.  Hardware shows both bars.
;
; Build and wrap in the timing-test boot disk:
;   VASM=/path/to/vasmm68k_mot ./build.sh dblpal-hires-lace
;
; Run on an AGA machine; the program takes over after the ROM loads it:
;   copperline --cpu 68EC020 --chipset AGA --chip 2M --noaudio \
;       --insert-disk-after 0 df0 dblpal-hires-lace.adf \
;       --screenshot-after 16 /tmp/dblpal-hires-lace.png

CUST        equ $dff000
BPL1        equ $40000
BPL2        equ $50000
CLIST       equ $60000
SPR0        equ $61000
SPR1        equ $61200
TERM        equ $61800

ROWS        equ 552
WORDS_ROW   equ 40              ; 640 one-bit SHRES samples
SPR_LINES   equ ($04a-$02a)/2   ; SSCAN2 repeats each fetched data row

        lea CUST,a6
        move.w #$7fff,$09a(a6)  ; all interrupts off
        move.w #$7fff,$09c(a6)  ; clear pending interrupts
        move.w #$7fff,$096(a6)  ; all DMA off while constructing the frame

        ; Plane 1: alternating single-sample columns.
        lea BPL1,a0
        move.w #ROWS-1,d0
.bpl1row:
        move.w #WORDS_ROW-1,d1
.bpl1word:
        move.w #$aaaa,(a0)+
        dbra d1,.bpl1word
        dbra d0,.bpl1row

        ; Plane 2: 16-row horizontal bands.  Together the planes make four
        ; colours, so horizontal and vertical scaling errors are both visible.
        lea BPL2,a0
        moveq #0,d2
        move.w #ROWS-1,d0
.bpl2row:
        move.w d2,d3
        lsr.w #4,d3
        and.w #1,d3
        neg.w d3                  ; 0 or $ffff
        move.w #WORDS_ROW-1,d1
.bpl2word:
        move.w d3,(a0)+
        dbra d1,.bpl2word
        addq.w #1,d2
        dbra d0,.bpl2row

        ; Sprite 0: the Workbench pointer's captured position.  POS=$2AB2 /
        ; CTL=$4A01 decodes to VSTART=42, VSTOP=74, HSTART=357.
        lea SPR0,a0
        move.w #$2ab2,(a0)+       ; POS
        move.w #$4a01,(a0)+       ; CTL
        move.w #SPR_LINES-1,d0
.spr0line:
        move.w #$ffff,(a0)+       ; DATA: colour 17
        clr.w (a0)+               ; DATB
        dbra d0,.spr0line
        clr.w (a0)+               ; descriptor terminator
        clr.w (a0)+

        ; Sprite 1: same vertical window, conservative HSTART=128.  This is
        ; the positive control proving that DMA and sprite output are active.
        lea SPR1,a0
        move.w #$2a40,(a0)+       ; POS: HSTART=128
        move.w #$4a00,(a0)+       ; CTL
        move.w #SPR_LINES-1,d0
.spr1line:
        clr.w (a0)+               ; DATA
        move.w #$ffff,(a0)+       ; DATB: colour 18
        dbra d0,.spr1line
        clr.w (a0)+
        clr.w (a0)+

        lea TERM,a0
        clr.w (a0)+
        clr.w (a0)+

        ; The Copper resets the display and sprite pointers every field.
        ; All other channels point at a shared empty descriptor.
        lea CLIST,a1
        move.l #$00e00004,(a1)+   ; BPL1PTH
        move.l #$00e20000,(a1)+   ; BPL1PTL
        move.l #$00e40005,(a1)+   ; BPL2PTH
        move.l #$00e60000,(a1)+   ; BPL2PTL
        move.l #$01200006,(a1)+   ; SPR0PTH
        move.l #$01221000,(a1)+   ; SPR0PTL
        move.l #$01240006,(a1)+   ; SPR1PTH
        move.l #$01261200,(a1)+   ; SPR1PTL
        moveq #5,d0               ; SPR2..SPR7 -> TERM
        move.w #$0128,d1
.sprptr:
        move.w d1,(a1)+
        move.w #$0006,(a1)+
        addq.w #2,d1
        move.w d1,(a1)+
        move.w #$1800,(a1)+
        addq.w #2,d1
        dbra d0,.sprptr
        move.l #$fffffffe,(a1)+

        ; DblPAL High Res Laced geometry captured from AmigaOS 3.1.
        move.w #$0081,$1c0(a6)   ; HTOTAL
        move.w #$0015,$1c2(a6)   ; HSSTOP
        move.w #$0001,$1c4(a6)   ; HBSTRT
        move.w #$0021,$1c6(a6)   ; HBSTOP
        move.w #$023d,$1c8(a6)   ; VTOTAL
        move.w #$000e,$1ca(a6)   ; VSSTOP
        move.w #$023e,$1cc(a6)   ; VBSTRT
        move.w #$0016,$1ce(a6)   ; VBSTOP
        move.w #$000b,$1de(a6)   ; HSSTRT
        move.w #$0007,$1e0(a6)   ; VSSTRT
        move.w #$004b,$1e2(a6)   ; HCENTER

        move.w #$0020,$092(a6)   ; DDFSTRT
        move.w #$0070,$094(a6)   ; DDFSTOP
        move.w #$2a5b,$08e(a6)   ; DIWSTRT
        move.w #$2afb,$090(a6)   ; DIWSTOP
        move.w #$0200,$1e4(a6)   ; DIWHIGH (must follow DIWSTRT/DIWSTOP)
        clr.w $108(a6)           ; BPL1MOD
        clr.w $10a(a6)           ; BPL2MOD
        clr.w $102(a6)           ; BPLCON1
        move.w #$0224,$104(a6)   ; BPLCON2: Workbench priority constellation
        move.w #$0c81,$106(a6)   ; BPLCON3: ECSENA/SPRES like the OS mode
        move.w #$0011,$10c(a6)   ; BPLCON4: AGA sprite palette bases
        move.w #$8003,$1fc(a6)   ; FMODE: SSCAN2 + BPL64

        move.w #$0012,$180(a6)   ; COLOR00: dark blue
        move.w #$0fff,$182(a6)   ; COLOR01: white
        move.w #$00f0,$184(a6)   ; COLOR02: green
        move.w #$0ff0,$186(a6)   ; COLOR03: yellow
        move.w #$0f00,$1a2(a6)   ; COLOR17: red sprite
        move.w #$00f0,$1a4(a6)   ; COLOR18: green control sprite

        move.w #$2245,$100(a6)   ; 2 planes, SHRES, LACE, ECSENA
        move.l #CLIST,$080(a6)   ; COP1LC
        move.w d0,$088(a6)       ; COPJMP1
        move.w #$83a0,$096(a6)   ; DMAEN|BPLEN|COPEN|SPREN

        ; Program the shorter beam last.  Once enabled, the Copper restarts
        ; the pointers at every programmable field boundary.
        move.w #$1b88,$1dc(a6)   ; BEAMCON0

.loop:
        bra.s .loop
