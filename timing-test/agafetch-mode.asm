; AGA wide-fetch word-address probe.
;
; A single lo-res plane displays three bands from the same repeating source:
;   $40..$77  FMODE 01, aligned BPL1PT: consecutive word pairs
;   $78..$AF  FMODE 10, aligned BPL1PT: first word duplicated
;   $B0..$E7  FMODE 01, BPL1PT+2: the supplied low address bit aliases both
;              phases of each 32-bit fetch
; The pointer advances four bytes in every band. BPL1MOD makes the 22-word
; standard row consume 24 source words so the eight-word pattern repeats.
; Cross-checked with vAmigaTS Agnus/Registers/FMODE/fmode10 and fmode11a-o.
;
; DO NOT re-bless this probe to match vAmiga.  vAmiga 5.0b1's new A1200_2MB
; AGA setup disagrees with Copperline over the FMODE 10 band exactly (rows
; $78..$AF, 17920 of 202628 pixels; the other two bands match to the pixel),
; and hardware sides with Copperline: running the vAmigaTS fmode10 test disk
; itself, Copperline reproduces the structure in that suite's real-A1200
; photograph, while vAmiga 5.0b1 renders magenta and white inside the
; staircase blocks (3.9% / 0.5% of the lower band) where the photograph has
; none.  vAmiga 5.0b1 reproduces the suite's committed fmode10_aga.raw
; byte-for-byte, so this is that reference's own behaviour, not a local build
; artefact.  Checked 2026-08-27.
CUST    equ $dff000
BMP     equ $40000
CLIST   equ $60000

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        ; Repeat an asymmetric eight-word pattern throughout the source.
        lea BMP,a0
        move.w #2048-1,d0
.fill:  move.w #$ffff,(a0)+
        clr.w (a0)+
        move.w #$aaaa,(a0)+
        move.w #$5555,(a0)+
        move.w #$f0f0,(a0)+
        move.w #$0f0f,(a0)+
        move.w #$cccc,(a0)+
        move.w #$3333,(a0)+
        dbra d0,.fill

        lea CLIST,a1
        move.l #$01800008,(a1)+   ; COLOR00 dark blue
        move.l #$01820fff,(a1)+   ; COLOR01 white
        move.l #$008e4081,(a1)+   ; DIWSTRT
        move.l #$0090e8c1,(a1)+   ; DIWSTOP
        move.l #$00920038,(a1)+   ; DDFSTRT
        move.l #$009400d0,(a1)+   ; DDFSTOP
        move.l #$01080004,(a1)+   ; BPL1MOD: 22 words + 2 = 24
        move.l #$010a0004,(a1)+
        move.l #$01001200,(a1)+   ; one lo-res plane

        move.l #$4007fffe,(a1)+
        move.l #$01fc0001,(a1)+   ; BPL32
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+

        move.l #$7807fffe,(a1)+
        move.l #$01fc0002,(a1)+   ; BPAGEM: duplicate first word
        move.l #$00e00004,(a1)+
        move.l #$00e20000,(a1)+

        move.l #$b007fffe,(a1)+
        move.l #$01fc0001,(a1)+   ; BPL32, pointer bit 1 already set
        move.l #$00e00004,(a1)+
        move.l #$00e20002,(a1)+

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$080(a6)
        move.w d0,$088(a6)
        move.w #$8380,$096(a6)   ; DMAEN|BPLEN|COPEN
.loop:  bra.s .loop
