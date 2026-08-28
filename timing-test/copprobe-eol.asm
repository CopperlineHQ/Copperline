; Copper line-end (E2) slot throughput probe.
;
; Agnus performs its fourth memory-refresh access on the last colour clock
; of the line ($E2 on PAL short lines). That cycle sits on the Copper's
; even-cck fetch grid, but it must NOT block a Copper fetch: the refresh
; RGA strobe overlaps a concurrent Copper transfer, so a saturated Copper
; stream keeps the clock. Together with the $E0 end-of-line lockout (a
; claimed cycle that fetches nothing) a PAL line offers the Copper 112
; fetchable clocks; blocking $E2 as a bus steal leaves 111 and starves any
; chain tuned to the real budget by one fetch per line (the Nexus 7
; plasma-zoom regression class: its per-band palette reload slips ~6 cck
; per 3-line band until a BPLCON4 sprite-bank flip lands mid-sprite).
;
; The probe runs five zones of 3-line bands. Every band is one WAIT plus a
; fixed MOVE stream (a bright and a dark COLOR00 marker embedded at fixed
; stream offsets); the zones step the stream length by one MOVE. A band
; that fits the per-band budget re-locks on its WAIT, so its markers land
; at the same beam position in every band: crisp vertical marker columns.
; A band that exceeds the budget falls through its WAIT ever later, so the
; markers precess to the right and down: a diagonal staircase. The zone at
; which columns give way to staircase measures the Copper's per-band slot
; budget, and losing the $E2 clock moves that boundary down a zone and
; steepens every staircase (2 extra slipped slots per band).
;
;   zone v      filler MOVEs  rendered columns
;   $30..$56    162           locked
;   $57..$7D    163           locked
;   $7E..$A4    164           locked (still locked with $E2 blocked)
;   $A5..$CB    165           locked <- flips to a staircase when the
;                                      line-end refresh steals $E2
;   $CC..$F2    166           staircase (steeper with $E2 blocked)
;
; A 3-line rest band (grey marker, ~8 slots) separates the zones so each
; zone starts re-synchronised regardless of the drift above it.
;
; vAmiga is NOT a valid reference for this probe: it marks the $E2 cycle
; as refresh-owned for every bus user, so its Copper drops to the smaller
; budget and slants the $A5 zone like the regression does (the Nexus 7
; chain that fits real hardware overruns under that model, and vAmiga
; 5.0b1 cannot run the demo to show it). WinUAE keeps all four refresh
; cycles off the Copper's grid and agrees with the larger budget.
CUST    equ $dff000
CLIST   equ $60000
ZONES   equ 5
BANDS   equ 12                    ; per zone, 3 lines each
FILL0   equ 162                   ; zone 0 filler MOVEs per band
PRE     equ 78                    ; fillers before the bright marker
MID     equ 26                    ; fillers between bright and dark
DARK    equ $0013
REST    equ $0333

        lea CUST,a6
        move.w #$7fff,$09a(a6)
        move.w #$7fff,$09c(a6)
        move.w #$7fff,$096(a6)

        move.w #$0200,$100(a6)    ; BPLCON0: no planes, colour on
        move.w #$0000,$106(a6)    ; BPLCON3: defaults, border shows COLOR00
        move.w #DARK,$180(a6)

        lea CLIST,a1
        move.l #$01000200,(a1)+   ; BPLCON0
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01040000,(a1)+   ; BPLCON2
        move.l #$01060000,(a1)+   ; BPLCON3
        move.w #$0180,(a1)+       ; COLOR00 dark until the first zone
        move.w #DARK,(a1)+

        lea brights(pc),a2
        moveq #0,d5               ; zone index
        move.w #$30,d7            ; band start line
zone:   move.w (a2)+,d4           ; this zone's bright marker colour
        move.w #FILL0,d3
        add.w d5,d3               ; fillers per band = FILL0 + zone
        moveq #BANDS-1,d6
band:   move.w d7,d0              ; WAIT (v, $01)
        lsl.w #8,d0
        or.w #$0001,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #PRE-1,d0          ; fillers, then the bright marker
        bsr.w fillers
        move.w #$0180,(a1)+
        move.w d4,(a1)+
        move.w #MID-1,d0          ; fillers, then back to dark
        bsr.w fillers
        move.w #$0180,(a1)+
        move.w #DARK,(a1)+
        move.w d3,d0              ; tail fillers
        sub.w #PRE+MID+1,d0
        bsr.w fillers
        addq.w #3,d7
        dbra d6,band
        move.w d7,d0              ; rest band: WAIT + grey + dark
        lsl.w #8,d0
        or.w #$0001,d0
        move.w d0,(a1)+
        move.w #$fffe,(a1)+
        move.w #$0180,(a1)+
        move.w #REST,(a1)+
        move.w #$0180,(a1)+
        move.w #DARK,(a1)+
        addq.w #3,d7
        addq.w #1,d5
        cmp.w #ZONES,d5
        blo.w zone

        move.l #$fffffffe,(a1)+
        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8280,$96(a6)     ; DMAEN|COPEN only
.l:     bra.s .l

fillers:                          ; d0 = count-1; emits MOVE #$0000,COLOR01
        move.w #$0182,(a1)+
        move.w #$0000,(a1)+
        dbra d0,fillers
        rts

brights: dc.w $0f00,$0fb0,$00f0,$00ee,$0e0e
