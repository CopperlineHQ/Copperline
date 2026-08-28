; BPLCON1 scroll fill on the first line of a bitplane-DMA block (hi-res,
; FMODE=0). Same display constellation as ddfprobe-hscroll (hi-res 1 plane,
; DDFSTRT $40, DDFSTOP $D0, DIWSTRT hstart $95: the fetch overhangs the
; window's left edge by 20 hi-res px), but the vertical DIW is held open
; from line 0 and each band's display is gated purely by a copper DMACON
; BPLEN write on the band's first line -- the Denise shifter sees no
; bitplane stream at all between bands. Six 8-line bands walk BPLCON1
; through $00,$22,$44,$66,$88,$AA; a seventh band (BPLCON1 $44, the
; Kickstart boot-screen scroll) is gated the other way, by DIWSTRT vstart
; reopening the closed vertical window with BPLEN already on.
;
; Readout: the scroll delay taps the shifter pixels loaded by the SAME
; line's pre-window fetch (all bands stay within the 20 px overhang, tap
; index >= 0), so on every line -- the band's FIRST line included -- the
; window's left edge must show the row-start marker sliver, identically in
; lines 1..8 of each band. A first line whose left edge shows background
; where lines 2..8 show the sliver means the emulator wrongly suppresses
; the same-line pre-fetch on a block-start line (the corner notches on the
; Super Skidmarks CD32 menu, whose two screen sections are BPLEN-gated
; exactly like this with DIW open all frame).
; Markers: w0=$FFFF w1=$F000 (row-start bar + tick, fetched before the
; window opens) and w36=w37=$FFFF (row-end bar clipped at the DIW stop).
CUST   equ $dff000
BMP    equ $40000
CLIST  equ $60000
NBAND  equ 6
        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)
        lea BMP,a0
        move.w #$ffff,(a0)+       ; w0: row-start bar
        move.w #$f000,(a0)+       ; w1: window-edge tick
        move.w #34-1,d0           ; w2..w35 empty
.z:     clr.w (a0)+
        dbra d0,.z
        move.w #$ffff,(a0)+       ; w36: row-end bar (head at the DIW stop)
        move.w #$ffff,(a0)+       ; w37
        move.w #8192-1,d0         ; keep the un-anchored scan area blank
.z2:    clr.w (a0)+
        dbra d0,.z2
        lea CLIST,a1
        move.l #$01009200,(a1)+   ; BPLCON0: 1 plane, hi-res
        move.l #$01020000,(a1)+   ; BPLCON1 = 0
        move.l #$01080000,(a1)+   ; BPL1MOD = 0
        move.l #$010a0000,(a1)+   ; BPL2MOD = 0
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$00920040,(a1)+   ; DDFSTRT $40 (KS boot-screen overhang)
        move.l #$009400d0,(a1)+   ; DDFSTOP $D0
        move.l #$008e0095,(a1)+   ; DIWSTRT: vstart 0 (open all frame)
        move.l #$0090a0ad,(a1)+   ; DIWSTOP: vstop 160, hstop $1AD
        move.l #$00e00004,(a1)+   ; BPL1PTH = $40000
        move.l #$00e20000,(a1)+   ; BPL1PTL
        lea scrolls(pc),a2
        moveq #NBAND-1,d2
        move.w #$3c00,d3          ; first band line $3C; WAIT hp = $07
.band:  move.w (a2)+,d4           ; BPLCON1 for this band
        moveq #8-1,d5
        moveq #1,d6               ; band's first line: raise BPLEN
.line:  move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+           ; WAIT (v,$07)
        move.w #$fffe,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1
        move.w d4,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w #$0000,(a1)+
        tst.w d6
        beq.s .noton
        move.w #$0096,(a1)+       ; DMACON: BPLEN on, well before DDFSTRT
        move.w #$8100,(a1)+
        moveq #0,d6
.noton: add.w #$0100,d3
        dbra d5,.line
        move.w d3,d0              ; line after the band: BPLEN off
        or.w #$0007,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0096,(a1)+
        move.w #$0100,(a1)+
        add.w #$0800,d3           ; next band 8 blank lines later
        dbra d2,.band
        move.w #$a407,d0          ; line 164: the window closed at vstop 160
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$008e,(a1)+       ; DIWSTRT: vstart 170 reopens the window
        move.w #$aa95,(a1)+
        move.w #$0096,(a1)+       ; BPLEN already on when it reopens
        move.w #$8100,(a1)+
        move.w #$aa00,d3          ; DIW-gated band lines 170..177
        moveq #8-1,d5
.dline: move.w d3,d0
        or.w #$0007,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0102,(a1)+       ; BPLCON1 $44
        move.w #$0044,(a1)+
        move.w #$00e0,(a1)+       ; BPL1PTH
        move.w #$0004,(a1)+
        move.w #$00e2,(a1)+       ; BPL1PTL
        move.w #$0000,(a1)+
        add.w #$0100,d3
        dbra d5,.dline
        move.w d3,d0              ; line 178: BPLEN off again
        or.w #$0007,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0096,(a1)+
        move.w #$0100,(a1)+
        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)         ; COPJMP1
        move.w #$8280,$96(a6)     ; DMAEN|COPEN; BPLEN stays copper-gated
.l:     bra.s .l
        cnop 0,2
scrolls: dc.w $0000,$0022,$0044,$0066,$0088,$00aa
