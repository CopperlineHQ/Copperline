; Bitplane fetch spill across the line end into a closed vertical window.
;
; Agnus's bitplane sequencer honours a DDFSTOP (or the $D8 hard stop) only
; at the next 8-colour-clock fetch-unit boundary, and the unit that starts
; there still runs as the last fetch unit. A hires window opened at DDFSTRT
; $3C with DDFSTOP $DC therefore has its last unit at $DC..$E3, one clock
; past the 227-clock PAL line: BPRUN is still set when the line ends and
; the run is carried into the next line, where the unit finishes before
; DDFSTRT ever matches. Inside the vertical window the carried tail is
; hidden past DIWSTOP.H and only the per-line word accounting shows it,
; but the vertical flop's reset on line DIWSTOP.V drops BPRUN with it, so
; the line after the last display line must fetch nothing at all.
;
; Copperline let the carried slot run on that closed line and painted the
; whole row from the memory past the plane (the AROS boot screen, whose
; amigavideo driver programs exactly this $3C/$DC hires window, showed a
; dashed teal line under its 256-line display on the ROM refreshed on
; 2026-09-05; vAmiga shows none).
;
; Display: two one-plane hires bands of 8-on/8-off stripes behind a solid
; 16-pixel bar in each row's first word (a hidden 42nd word of $0f0f ends
; each row of the overrunning band), each plane followed by four rows of
; all-ones marker words so that memory past the plane is never blank:
;
;   top      DDFSTRT=$3C DDFSTOP=$DC (last unit overruns the line)
;            BPL1PT=PLANE_A (84-byte rows: 42 words fetched per line)
;            DIWSTRT=$9081 DIWSTOP=$C8C1         -> band 144..199
;   v208     DDFSTOP=$D4 (standard hires stop: no overrun, control)
;            BPL1PT=PLANE_B (80-byte rows: 40 words fetched per line)
;            DIWSTRT=$D881 DIWSTOP=$10C1         -> band 216..271
;
; Expected settled render: stripes over 144..199 and 216..271, border on
; every other line. The standard band's bar is a straight vertical line;
; the overrunning band's bar walks right one word per row, because the
; sequencer (like vAmiga's) consumes 41 words per line from the 42-word
; rows: the carried unit's final slot re-runs the previous slot on the new
; line and the plane's word falls to the refresh cycle, so the pointer
; advances 82 bytes per line. That drift is the per-line accounting made
; visible and is part of the golden. Any content on line 200 is the carried
; run surviving the closed flop (Copperline repeated the band's last row
; there); any on 272 would be a fetch spilling from the standard window.
;
; Cross-checked against vAmiga (tools/vamiga-ref.sh).
CUST    equ $dff000
PLANE_A equ $40000
PLANE_B equ $48000
CLIST   equ $60000
ROWS    equ 56
MARKS   equ 4

        lea CUST,a6
        move.w #$7fff,$9a(a6)
        move.w #$7fff,$9c(a6)
        move.w #$7fff,$96(a6)

        ; plane A: ROWS rows of [$ffff bar, 40 stripe words, $0f0f hidden
        ; word], then MARKS rows of $ffff. The bar pins each row's first
        ; word: a run fetching one word too few or too many per line walks
        ; it sideways one word per row.
        lea PLANE_A,a0
        move.w #ROWS-1,d1
.ra:    move.w #$ffff,(a0)+
        moveq #40-1,d0
.rw:    move.w #$ff00,(a0)+
        dbra d0,.rw
        move.w #$0f0f,(a0)+
        dbra d1,.ra
        move.w #MARKS*42-1,d0
.ma:    move.w #$ffff,(a0)+
        dbra d0,.ma

        ; plane B: ROWS rows of [$ffff bar, 39 stripe words], then MARKS
        ; rows of $ffff
        lea PLANE_B,a0
        move.w #ROWS-1,d1
.rb:    move.w #$ffff,(a0)+
        moveq #39-1,d0
.rx:    move.w #$ff00,(a0)+
        dbra d0,.rx
        dbra d1,.rb
        move.w #MARKS*40-1,d0
.mb:    move.w #$ffff,(a0)+
        dbra d0,.mb

        ; ---- copper list ----
        lea CLIST,a1
        move.l #$01009200,(a1)+   ; BPLCON0: hires, 1 plane, colour
        move.l #$01020000,(a1)+   ; BPLCON1
        move.l #$01080000,(a1)+   ; BPL1MOD: rows are exactly what is fetched
        move.l #$0092003c,(a1)+   ; DDFSTRT
        move.l #$009400dc,(a1)+   ; DDFSTOP: one unit past the $D8 hard stop
        move.l #$01800113,(a1)+   ; COLOR00 dark blue border/background
        move.l #$01820fff,(a1)+   ; COLOR01 white stripes
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE_A
        move.l #$00e20000,(a1)+
        move.l #$008e9081,(a1)+   ; DIWSTRT: open at 144
        move.l #$0090c8c1,(a1)+   ; DIWSTOP: close at 200
        move.l #$d0010000+$fffe,(a1)+ ; WAIT v208
        move.l #$009400d4,(a1)+   ; DDFSTOP: standard hires stop
        move.l #$00e00004,(a1)+   ; BPL1PT = PLANE_B
        move.l #$00e28000,(a1)+
        move.l #$008ed881,(a1)+   ; DIWSTRT: open at 216
        move.l #$009010c1,(a1)+   ; DIWSTOP: close at 272 (V8 form)
        move.l #$fffffffe,(a1)+

        move.l #CLIST,$80(a6)
        move.w d0,$88(a6)
        move.w #$8380,$96(a6)     ; DMAEN|BPLEN|COPEN
.l:     bra.s .l
