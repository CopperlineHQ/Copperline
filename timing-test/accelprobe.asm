; Accelerator CPU timing probe: 020/030/040/060 exception, cache-op, and
; per-RAM-region costs, measured against the CIA-A E-clock like the main
; timing-test disk. Motivation: the boot-time MMU table build that
; SetPatch/68040.library/mmu.library performs walks EVERY 4K page of fitted
; RAM (several library calls, a Supervisor() round trip, and a CPUSHL per
; page), so its cost scales with RAM size; this probe isolates each primitive
; of that walk plus the raw read/write rate of every fast-RAM region the
; machine actually has (CPU-card, motherboard, Zorro III), so an emulator's
; billing of each piece can be checked against real silicon. The composite
; rows (27-29) replicate the per-page sequence observed in a real
; mmu.library trace.
;
; Boot on real hardware from a cold start (the probe trashes OS structures
; in fast RAM; warm reboot after a run is not expected to work). Capture the
; SERIAL stream (9600 8N1) for the numbers -- typed CRT transcription only as
; a fallback, never digits read off a photo by eye.
;
; The probe requires a 68010+ (VBR); on a 68020+ it sets a defined cache
; state itself (see below). All rows run in SUPERVISOR mode (SuperState()),
; so the TRAP rows measure the supervisor->supervisor exception round trip;
; a real mmu walk traps from user mode, whose entry differs only in the
; stacked SR's S bit and costs the same bus traffic.
;
; Cache state set before the rows (and echoed in row 4):
;   68020: CACR = $00000009 written (CI|EI)            -> reads back $0001
;   68030: CACR = $00003919 written (WA|DBE|CD|ED|IBE|CI|EI) -> $3111
;   68040: CPUSHA BC; TC/ITT0/ITT1/DTT0/DTT1 = 0; CACR = $80008000
;   68060: as 68040, plus PCR.ESS (superscalar) and CACR EBC ($80808000)
; With the MMU off and no TTx match, 040/060 default accesses are cachable
; WRITE-THROUGH, so the write rows measure the memory write rate, not
; copyback allocation.
;
; Rows (values are E-clock ticks unless stated; 0 = skipped/absent,
; $FFFFFFFF = the instruction faulted -- an emulator-support signal):
;   row 0  signature $ACCE1B02 (v1 captures read $ACCE1B01)
;   row 1  detected CPU: $20 / $30 / $40 / $60 (from live probes, not exec)
;   row 2  exec AttnFlags (raw word, captured before takeover)
;   row 3  68060 PCR readback (else 0)
;   row 4  CACR readback after the enable block
;   row 5  original VBR at entry
;   row 6  move.w d2,d0                     x4096  (calibration anchor)
;   row 7  dbra-only loop                   x4096
;   row 8  mulu #$5555,d5                   x4096  (main-disk row 6 anchor)
;   row 9  trap #0 + rte                    x1024, SSP in chip RAM
;   row 10 trap #0 + rte                    x1024, SSP in fast region 1
;   row 11 jsr/rts, target in chip (near)   x4096
;   row 12 move.l (a2),a3 + jsr (a3)        x4096  (call through a vector)
;   row 13 jsr/rts, target in fast region 1 x4096  (cross-region, I-cached)
;   row 14 movem.l 11 regs push+pop         x1024, SP in fast region 1
;   row 15 cpushl dc,(a0) stride-16 walk    x4096  over fast region 1 (040/060)
;   row 16 chip  read  move.l stride 16     x4096  (64K window, line-fill rate)
;   row 17 chip  write move.l stride 16     x4096
;   row 18 fast region 1 base address (MemList order, first three
;          regions displayed; 0 = absent)
;   row 19 fast1 read  move.l stride 16     x4096
;   row 20 fast1 write move.l stride 16     x4096
;   row 21 fast region 2 base address
;   row 22 fast2 read
;   row 23 fast2 write
;   row 24 fast region 3 base address
;   row 25 fast3 read
;   row 26 fast3 write
;   row 27 composite page walk, FULL        x512 pages (040/060) over the
;          LARGEST fast region of the whole MemList (tracked even past the
;          three displayed regions; stack in fast region 1, so rows 27-29
;          are skipped when region 1 cannot host it):
;          movem push/pop + 2 vector calls + trap/rte + page read +
;          descriptor write + cpushl, one 4K page per iteration
;   row 28 composite, no trap               x512  (27-28 isolates the trap)
;   row 29 composite, no cpushl             x512  (every CPU: the cross-CPU
;          comparable walk; 27-29 isolates the cpushl on 040/060)
;   row 30 bare page step tst.l + 4K adda   x512
;   row 31 E-clock ticks per video frame (PAL ~28375)
;
; Copperline PREDICTED columns, m68k 0.11 -- HISTORICAL: the model every
; VERDICTS block below was written against, kept so those verdicts stay
; readable; the m68k 0.12 columns follow this table. (v2 binary
; $ACCE1B02, A4000 profile, PAL, tt-a4000-060/-040/-030.toml: 2M
; motherboard RAM mirroring the real machine -- Copperline bases it
; top-down at $07E00020 exactly like real Ramsey -- plus 128M Z3; 060 also
; 64M CPU-card RAM). The v2 composite walks the LARGEST fast region (Z3
; here); Copperline's numbers match v1's region-1 walk because it bills
; every fast class identically.
;   row    060@50MHz  040@25MHz  030@25MHz   row    060       040       030
;   06 reg 00000040   00000498   00000498    16 crd 000009B4  00000CDF  00000CDF
;   07 dbr 0000003E   000003AC   000003AC    17 cwr 00000671  00000671  00000673
;   08 mul 000000B4   00001003   00001003    19 f1r 00000078  00000927  00000929
;   09 trC 0000101C   000011BC   000011BB    20 f1w 00000078  0000083D  0000083D
;   10 trF 0000033A   00000671   00000672    22 f2r 00000078  00000927  00000929
;   11 jsr 00000CE0   00001029   00001029    23 f2w 00000078  0000083D  0000083D
;   12 jsv 0000168A   000019BA   000019BA    25 f3r 00000078  0 absent  0 absent
;   13 jsf 00000CE0   00001028   00001028    26 f3w 00000078  0 absent  0 absent
;   14 mvm 0000099D   00001421   00001421    27 cfu 0000073D  00000FA8  0 skip
;   15 cpl 0000007A   0000066A   0 skip      28 cnt 00000614  00000CDB  0 skip
;   30 stp 00000013   0000010B   0000010B    29 cnc 0000073C  00000FA5  00000FA6
;   31 frame length: 00003780/1 on all three (PAL, matches the main disk)
;   Fast regions: 060 f1=accel $08000020, f2=mb $07E00020, f3=Z3 $40000020;
;   040/030 (no CPU-card RAM) f1=mb $07E00020, f2=Z3, f3 absent.
;
; Copperline PREDICTED columns, m68k 0.12 (same binary and configs; the
; 030 now runs the MC68020UM tables and the 040 its single-issue pipeline
; model, both calibrated on the real columns below -- docs/internals/cpu.md
; "68020 timing" / "68040 timing"; the 060 model is unchanged):
;   row    060@50MHz  040@25MHz  030@25MHz   row    060       040       030
;   06 reg 00000040   000001DA   000003AE    16 crd 000009B4  000009B4  00000CE1
;   07 dbr 0000003E   00000163   00000337    17 cwr 00000671  00000671  00000671
;   08 mul 000000B4   000005F5   00001078    19 f1r 00000078  0000050A  000007CA
;   09 trC 0000101C   000010E9   000011BC    20 f1w 00000078  0000050A  0000083D
;   10 trF 0000033A   0000059D   0000073A    22 f2r 00000078  0000050A  000007CA
;   11 jsr 00000CE0   00000CE0   00001029    23 f2w 00000078  0000050A  0000083D
;   12 jsv 0000168A   0000168A   000019BA    25 f3r 00000078  0 absent  0 absent
;   13 jsf 00000CE0   00000CDF   00001028    26 f3w 00000078  0 absent  0 absent
;   14 mvm 0000099D   0000138F   000013E7    27 cfu 0000073D  00000E71  0 skip
;   15 cpl 0000007A   00000754   0 skip      28 cnt 00000613  00000BA2  0 skip
;   30 stp 00000013   000000A5   000000FD    29 cnc 0000073C  00000DA5  00000EDC
;   31 frame length: 00003780/1 on all three, as before.
;   vs the real columns below: 040 row 6 is tick-exact ($1DA), rows 8 and
;   15 within 7%, row 7 3.0 vs 4.06 clk (the model's documented compromise:
;   no overlap stage, so the empty dbra loop under-reads while the
;   one-clock-body loop is exact); the 040 trap rows stay 1.4-1.7x under
;   (exception entries keep the legacy scaled costs) and movem 1.4x over.
;   030 rows 6/8/10 within 1%, row 7 1.18x over (was 1.35x); its call,
;   movem and chip-stack trap rows are bus-bound and unchanged, its chip
;   rows stay tick-exact. The memory rows (16-26) and the composite walk
;   move only by the cheaper loop overhead: the region/bridge billing
;   itself is unchanged, so on the 040 the chip-over-bridge read under-bill
;   widens from 3x to 4x and the Z3 read from 2.6x to 4.8x, while the
;   030's motherboard read goes from 1.12x over to 5% under.
;
; REAL A4000 + BFG9060 (68060 rev 5 @ 50 MHz, PCR $04300502 with DFP set at
; entry, AttnFlags $000F, VBR 0), CPU-card RAM f1=$08000020, 2M motherboard
; RAM f2=$07E00020 (top-down: only 2M fitted), ZZ9000 Z3 RAM f3=$50000020.
; Two serial captures 2026-08-29, tick-identical except where marked ~
; (Z3 read jitters ~11 ticks between runs; frame row phase +-18):
;   06 reg 00000077   09 trC 00002459   14 mvm 000005F1   22 f2r 00000DD8
;   07 dbr 0000003C   10 trF 000004D3   15 cpl 00000501   23 f2w 00000495
;   08 mul 000000B0   11 jsr 00002696   16 crd 00001D14   25 f3r 00001721~
;   30 stp 00000063   12 jsv 00004399   17 cwr 000009B2   26 f3w 0000065E
;   27 cfu 00000E78   13 jsf 00002696   19 f1r 000002FB   31 frm 0000378A~
;   28 cnt 00000C0F   29 cnc 00000DBF   20 f1w 00000212
; Internal consistency: rows 11 and 13 identical (cached call target's
; region is irrelevant); rows 27-28 = 617 ticks/512 pages = 1.70us/trap =
; row 10 exactly; rows 27-29 = 185/512 = 0.51us/cpushl ~ row 15's 0.44us.
; VERDICTS vs the m68k 0.11 Copperline predicted column (060@50; the 060
; model is unchanged in 0.12, so these still stand):
;   - Fast RAM is NOT one class on real silicon: line-read cost CPU-card
;     263ns / motherboard 1.22us / ZZ9000 Z3 2.04us per 16 bytes, where
;     Copperline bills all three 41ns (6x / 30x / 50x under-billed).
;   - Chip access over the CPU-card bridge: real move.l read ~2.5us (!!),
;     write ~850ns; Copperline ~855ns/570ns (3x / 1.5x under). This also
;     explains rows 9/11/12/13: their stack or data sits in chip.
;   - trap+rte (fast stack) real 1.70us = 85 clk vs CL 1.14us = 57 clk.
;   - cpushl real 441ns = 22 clk vs CL 42ns (10x under, as predicted).
;   - movem 11 regs push+pop: real 2.09us vs CL 3.39us -- the one place
;     Copperline OVER-bills (1.6x).
;   - move.w d2,d0 + dbra: real 2.05 clk/iter (no dual-issue fold); CL 1.10.
;   - dbra/mulu/frame rows within 3% -- E-clock harness and branch cache OK.
;   - Composite walk: real 10.2us/page vs CL 5.1us/page: the boot MMU page
;     walk is UNDER-billed 2x on the 060, not over-billed.
;   (The column above is the v1 binary: its composite walked region 1,
;    the BFG9060's CPU-card RAM, at 10.2us/page.)
; BFG9060 v2 run (composite walks the ZZ9000 Z3 region; rows 6-26 match
; v1 within jitter, Z3 read $1714~):
;   27 cfu 00001143 = 12.2us/page   29 cnc 00001084 = 11.6us/page
;   28 cnt 00000EC0 = 10.4us/page   30 stp 00000316 =  2.18us/page
; Deltas reproduce rows 10/15 (1.77us/trap, ~526ns/cpushl); row 30 ~ one
; Z3 line fill. Verdict: real 060 Z3 walk 12.2us/page vs Copperline's
; 5.1us/page = UNDER-billed 2.4x; only 1.4x faster than the real 040's
; 17.6us/page despite twice the clock -- the walk is bus-bound.
;
; REAL A4000 + Commodore A3640 (68040 @ 25 MHz, no local RAM, MAPROM
; enabled -- irrelevant to every row: after takeover the probe executes no
; ROM, its vectors and handlers live in chip RAM under its own VBR).
; AttnFlags $804F = the emulated value, VBR 0; f1 = 2M motherboard
; $07E00020 (identical base to the emulated machine), f2 = ZZ9000 Z3
; $50000020. TWO runs BYTE-IDENTICAL on every row including the frame row
; -- fully deterministic capture. The 3.4us chip read below is the A3640's
; famously slow chipset coupling, now quantified. The v1 runs' rows 27-30
; were 0 (v1's composite guard wanted region 1 >= 2.1M and the 2M
; motherboard bank fails it); the v2 re-run below walks the ZZ9000 Z3
; region and supplies the composite -- THE number for the 040 boot-walk
; question.
;   06 reg 000001DA   10 trF 000009AD   15 cpl 000006D9   20 f1w 0000042B
;   07 dbr 000001D8   11 jsr 00003077   16 crd 000026C3   22 f2r 00001832
;   08 mul 0000065E   12 jsv 00005730   17 cwr 000009B7   23 f2w 000005EF
;   09 trC 00001844   13 jsf 00003075   19 f1r 00000ECD   31 frm 00003785
;   14 mvm 00000DA8
; A3640 v2 run (composite walks the ZZ9000 Z3 region; rows 6-26 match the
; v1 runs within jitter, Z3 read $1848~):
;   27 cfu 000018FA = 17.6us/page   29 cnc 00001811 = 17.0us/page
;   28 cnt 00001462 = 14.4us/page   30 stp 0000032B =  2.23us/page
; Internal consistency: 27-28 = 3.24us/trap ~ row 10's 3.41us; 27-29 =
; 641ns/cpushl ~ row 15's 603ns; row 30 ~ one Z3 line fill (row 22).
; Verdict: real A3640 walking Z3 = 17.6us/page vs Copperline's predicted
; 11.0us/page (row 27 $FA8): the 040 MMU boot walk over Z3 is
; UNDER-billed 1.6x -- a real 512M Z3 board costs ~2.3s per walk pass on
; real silicon. Combined with the ~2x plain-execution over-bill, the
; emulated AmigaVision 040 boot is slow for the WRONG reasons: the CPU
; share should shrink ~2x and the walk share should grow ~1.6x.
; VERDICTS vs the m68k 0.11 Copperline predicted 040@25 column
; (HISTORICAL: the 0.12 pipeline model closes the CPU-core items, see the
; 0.12 table above) -- the direction INVERTS on the CPU core:
;   - Plain execution is OVER-billed ~2-2.5x: reg+dbra real 4.1 clk/iter
;     vs CL 10.1; bare taken dbra real ~4 clk vs CL ~8; mulu.w real ~14
;     clk (databook figure) vs CL ~35. Copperline's 040 runs plain code
;     about half real speed -- the substance behind "boots feel slow on
;     an 040" for CPU-bound workloads.
;   - movem over-billed 1.5x (real 4.81us vs CL 7.09us per push+pop).
;   - trap+rte fast stack: real 3.41us = 85 clk (the SAME clock count as
;     the real 060) vs CL 57 clk: 1.5x under, matching the 060 gap.
;   - Chip bridge: move.l read real 3.4us (!) vs CL 1.13us (3x under);
;     write 856ns vs 568ns (1.5x) -- same bridge costs as the 060 card.
;   - Motherboard RAM: read 1.6x under (real 1.30us/line vs CL 806ns),
;     write 2x OVER (real 367ns vs CL 726ns). Z3: read 2.6x under (real
;     2.13us/line), write 1.4x over. So the 040 model's RAM billing is
;     much closer than the 060's flat 41ns, but reads and writes err in
;     opposite directions.
;   - cpushl real 603ns = 15 clk vs CL 565ns: within 7% (only the 060
;     model has the near-free cpushl).
;   - Cross-card validation: motherboard and Z3 read/write rates and the
;     chip write cost measured from the 040 card agree with the 060
;     card's within ~7%: they are properties of the region/bridge, not
;     the CPU, exactly as a per-region timing model would want.
;
; REAL A4000 + Commodore 68030 CPU-slot board @ 25 MHz (v1 binary;
; AttnFlags $0007 -- no FPU bits -- and CACR readback $3111 both EXACTLY
; the emulated values; f1 = 2M motherboard $07E00020, f2 = ZZ9000 Z3
; $50000020). Two runs within 1-4 ticks:
;   06 reg 000003A5   10 trF 00000747   14 mvm 00000DA1   20 f1w 00000573
;   07 dbr 000002BB   11 jsr 00000992   16 crd 00000CDE   22 f2r 00000AA9
;   08 mul 0000105A   12 jsv 00001028   17 cwr 00000671   23 f2w 00000573
;   09 trC 00000E88   13 jsf 00000991   19 f1r 0000082E   31 frm 0000377C
; VERDICTS vs the m68k 0.11 Copperline predicted 030@25 column
; (HISTORICAL: 0.12 moves the 030 onto the MC68020UM tables, see the 0.12
; table above) -- the closest model of the three:
;   - Chip RAM is TICK-EXACT: read real $CDE vs CL $CDF, write $671/$671.
;     The 3x chip gap on the 040/060 cards is their CPU-slot bridge, not
;     the motherboard path.
;   - mulu.w is exact (36.0 clk real vs 35.3 CL); reg+dbra 1.26x over
;     (real 8.0 clk/iter), bare taken dbra 1.35x over (real 6.0 clk --
;     the same 6-clk figure the real-A1200 020 campaign measured).
;   - Fast RAM close: mb read 1.12x over (real 721ns), Z3 read 1.16x
;     under (real 939ns); both writes 1.5x over (real 480ns each).
;     NOTE: on the 030 a read row moves ONE longword per stride (no
;     16-byte line fill), so per-LONG the ZZ9000 is still slower than
;     the 040/060 line-fill numbers imply (939ns/long vs ~533ns/long).
;   - Calls over-billed 1.6-1.7x (real jsr/rts+dbra 21 clk chip-stack);
;     movem 1.5x over (real 4.80us, same figure as the real 040);
;     trap+rte: chip-stack 1.2x OVER (real 5.12us), fast-stack 1.13x
;     under (real 2.56us = 64 clk vs 85 on real 040/060).
;   - Composite rows 0 on this machine (v1 guard + <040 cpushl skips);
;     row 29 (no-cpushl walk) needs a v2 re-run to land.
; Notable m68k 0.11 predictions the real columns tested: Copperline billed
; the 030 and 040 identically on most CPU rows (no longer so in 0.12),
; bills all three fast-RAM classes identically (real Z3 over a Zorro III
; bus is far slower than CPU-card RAM; still open), and its 060 CPUSHL is
; nearly free (the cache-op is modelled as an invalidate; the 040 model
; bills ~12 clocks). The trap rows and the composite walk are the direct
; check on the boot-time MMU page-walk cost that scales with fitted RAM.
;
; Scratch chip addresses (all below the 2M A4000 chip ceiling):
;   $30000 program (loaded by boot.asm)   $40000 screen (1 plane)
;   $48000 results                        $4E000 vector table (VBR)
;   $4F800 captured region table          $58000 composite descriptor table
;   $5E000 supervisor stack top           $60000-$70000 chip test window
; Fast-region scratch (W = region base + $10000): W..W+$10000 test window,
; W+$18000 trap/movem stack top, W+$1C000 jsr target; regions smaller than
; $30000 are skipped, the composite needs base+$210000 and uses W..W+2M.

CUST    equ     $dff000
SCREEN  equ     $40000
RESULTS equ     $48000
VECTBL  equ     $4e000
REGTBL  equ     $4f800          ; 3 x (lower.l, upper.l)
REGCNT  equ     $4f818          ; word: fast regions captured
ATTNF   equ     $4f81a          ; word: exec AttnFlags
FAULTF  equ     $4f81c          ; word: set by the skip handlers
CPUTYP  equ     $4f81e          ; word: $20/$30/$40/$60
SAVSP   equ     $4f820          ; long: SP save for the fast-stack rows
LARGLO  equ     $4f824          ; long: largest fast region, MH_LOWER
LARGHI  equ     $4f828          ; long: largest fast region, MH_UPPER
DESCT   equ     $58000
SSTACK  equ     $5e000
CHIPW   equ     $60000

ITER4K  equ     4096
ITER1K  equ     1024
PAGES   equ     512

LVOSuperState equ -150

;----------------------------------------------------- entry (a6=sys, user mode)
boot:
        ; Capture AttnFlags and the fast regions from the exec MemList while
        ; the OS is still alive: MH_ATTRIBUTES/_LOWER/_UPPER of every
        ; MEMF_FAST header, in MemList (priority) order. The region table is
        ; how one binary adapts to whichever accelerator/RAM mix is fitted.
        move.w  296(a6),ATTNF   ; ExecBase->AttnFlags
        lea     REGTBL,a0
        moveq   #0,d3
        clr.l   LARGLO          ; largest fast region seen so far
        clr.l   LARGHI
        move.l  322(a6),a1      ; ExecBase->MemList lh_Head
.ml     move.l  (a1),d0         ; ln_Succ (0 on the tail node: done)
        beq     .mld
        move.w  14(a1),d1       ; MH_ATTRIBUTES
        and.w   #4,d1           ; MEMF_FAST
        beq     .mln
        cmp.w   #3,d3
        bhs     .mlg            ; table full: still track the largest
        move.l  20(a1),(a0)+    ; MH_LOWER
        move.l  24(a1),(a0)+    ; MH_UPPER
        addq.w  #1,d3
.mlg    move.l  24(a1),d1       ; the composite walks the LARGEST fast
        sub.l   20(a1),d1       ; region of the whole list, not only the
        move.l  LARGHI,d2       ; three the base/read/write rows display
        sub.l   LARGLO,d2
        cmp.l   d2,d1
        bls     .mln
        move.l  20(a1),LARGLO
        move.l  24(a1),LARGHI
.mln    movea.l d0,a1
        bra     .ml
.mld    move.w  d3,REGCNT

        ; Enter supervisor mode for good. SuperState() keeps the current
        ; stack and returns to the caller in supervisor state, so no RTE
        ; tricks and no assumption about where VBR points.
        jsr     LVOSuperState(a6)

        lea     CUST,a6
        move.w  #$7fff,$9a(a6)  ; INTENA: all off
        move.w  #$7fff,$9c(a6)  ; INTREQ: clear
        move.w  #$7fff,$96(a6)  ; DMACON: all off
        move.w  #$0f00,$180(a6) ; border red: alive, tests running

        ; clear the screen bitplane
        lea     SCREEN,a0
        move.w  #(40*256/4)-1,d0
.clrs   clr.l   (a0)+
        dbra    d0,.clrs

        ; The program is loaded at $30000 but assembled at origin 0, so the
        ; composite's call-vector block must be patched with runtime
        ; addresses (assemble-time dc.l would store file offsets).
        lea     vecblk(pc),a1
        lea     stub1(pc),a0
        move.l  a0,(a1)+
        lea     stub2(pc),a0
        move.l  a0,(a1)

        ; This is an accelerator probe: it needs the 020+ MOVEC set and a
        ; cache to put into a defined state. A 68000 has neither; a 68010
        ; would pass a VBR check but then misreport as a $20 after both
        ; feature probes fault, so gate on the 68020/030/040 AttnFlags
        ; bits and send everything below them to the signature-only path.
        move.w  ATTNF,d0
        and.w   #$000e,d0       ; AFB_68020|68030|68040
        bne     .cpu_ok
        bra     unsupported
.cpu_ok

        ; Build our own vector table and point VBR at it: every entry ->
        ; fatal (magenta border, halt), then the handlers we actually use.
        ; This makes the probe independent of wherever the boot ROM left the
        ; vectors and gives TRAP #0 a fixed-cost handler.
        lea     VECTBL,a0
        move.w  #256-1,d0
        lea     fatal(pc),a1
.vt     move.l  a1,(a0)+
        dbra    d0,.vt
        lea     t0rte(pc),a1
        move.l  a1,VECTBL+$80   ; TRAP #0
        dc.w    $4e7a,$0801     ; movec vbr,d0 (68010+)
        move.l  d0,-(sp)        ; original VBR: rendered as row 5
        move.l  #VECTBL,d0
        dc.w    $4e7b,$0801     ; movec d0,vbr
        move.l  (sp)+,d5        ; d5 = original VBR, stored below

        ; new supervisor stack in chip RAM, clear of everything we touch
        movea.l #SSTACK,sp

        ; --- identify the CPU by probing, not by trusting exec ------------
        ; movec pcr (068060 only) else movec itt0 (68040) else AttnFlags
        ; 030/020. The skip handler steps the stacked PC over the faulting
        ; 4-byte movec.
        lea     mskip4(pc),a1
        move.l  a1,VECTBL+$10   ; illegal instruction
        move.l  a1,VECTBL+$2c   ; F-line
        move.w  #$60,CPUTYP
        clr.w   FAULTF
        moveq   #0,d0
        dc.w    $4e7a,$0808     ; movec pcr,d0
        move.l  d0,d6           ; d6 = PCR (0 if faulted): row 3
        swap    d0
        cmp.w   #$0430,d0       ; 68060 identification
        beq     .id_done
        move.w  #$40,CPUTYP
        clr.w   FAULTF
        moveq   #0,d0
        dc.w    $4e7a,$0004     ; movec itt0,d0 (68040/060 only)
        ; if the read did NOT fault, FAULTF is still 0: a 68040
        tst.w   FAULTF
        beq     .id_done
        clr.w   FAULTF
        move.w  #$30,CPUTYP
        move.w  ATTNF,d0
        btst    #2,d0           ; AFB_68030
        bne     .id_done
        move.w  #$20,CPUTYP
.id_done

        ; --- defined cache state ------------------------------------------
        move.w  CPUTYP,d0
        cmp.w   #$40,d0
        blo     .c2030
        ; 040/060: flush+invalidate both caches, MMU hard off, caches on
        dc.w    $f4f8           ; cpusha bc
        moveq   #0,d0
        dc.w    $4e7b,$0003     ; movec d0,tc
        dc.w    $4e7b,$0004     ; movec d0,itt0
        dc.w    $4e7b,$0005     ; movec d0,itt1
        dc.w    $4e7b,$0006     ; movec d0,dtt0
        dc.w    $4e7b,$0007     ; movec d0,dtt1
        move.l  #$80008000,d0   ; CACR: EDC | EIC
        cmp.w   #$60,CPUTYP
        bne     .cset
        move.l  d6,d1           ; PCR with ESS on: superscalar dispatch
        bset    #0,d1
        dc.w    $4e7b,$1808     ; movec d1,pcr
        move.l  #$80808000,d0   ; CACR: EDC | EBC | EIC
        bra     .cset
.c2030  cmp.w   #$30,d0
        bne     .c20
        move.l  #$00003919,d0   ; WA|DBE|CD|ED|IBE|CI|EI (CD/CI are strobes)
        bra     .cset
.c20    move.l  #$00000009,d0   ; CI|EI
.cset   dc.w    $4e7b,$0002     ; movec d0,cacr
        moveq   #0,d0
        dc.w    $4e7a,$0002     ; movec cacr,d0
        move.l  d0,d7           ; d7 = CACR readback: row 4

        ; --- self-description rows 0-5 ------------------------------------
        lea     RESULTS,a3
        move.l  #$acce1b02,(a3)+        ; row 0 signature (v2: composite
                                        ; walks the LARGEST fast region)
        moveq   #0,d0
        move.w  CPUTYP,d0
        move.l  d0,(a3)+                ; row 1 CPU
        moveq   #0,d0
        move.w  ATTNF,d0
        move.l  d0,(a3)+                ; row 2 AttnFlags
        move.l  d6,(a3)+                ; row 3 PCR
        move.l  d7,(a3)+                ; row 4 CACR readback
        move.l  d5,(a3)+                ; row 5 original VBR

        ; --- rows 6-8: CPU anchors ----------------------------------------
        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t6     move.w  d2,d0
        dbra    d6,.t6
        bsr     tread
        move.l  d0,(a3)+                ; row 6

        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t7     dbra    d6,.t7
        bsr     tread
        move.l  d0,(a3)+                ; row 7

        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t8     mulu    #$5555,d5
        dbra    d6,.t8
        bsr     tread
        move.l  d0,(a3)+                ; row 8

        ; --- rows 9/10: trap round trip, chip vs fast SSP -----------------
        cnop    0,4
        bsr     tstart
        move.w  #ITER1K-1,d6
.t9     trap    #0
        dbra    d6,.t9
        bsr     tread
        move.l  d0,(a3)+                ; row 9 (SSP in chip)

        bsr     fast1w                  ; d0 = fast1 window, or 0
        tst.l   d0
        beq     .r10s
        move.l  sp,SAVSP
        movea.l d0,sp
        adda.l  #$18000,sp              ; SSP -> fast region 1
        cnop    0,4
        bsr     tstart
        move.w  #ITER1K-1,d6
.t10    trap    #0
        dbra    d6,.t10
        bsr     tread
        movea.l SAVSP,sp
        move.l  d0,(a3)+                ; row 10
        bra     .r10d
.r10s   clr.l   (a3)+
.r10d

        ; --- rows 11-13: calls --------------------------------------------
        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t11    bsr     nearrts
        dbra    d6,.t11
        bsr     tread
        move.l  d0,(a3)+                ; row 11

        lea     vecblk(pc),a2
        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t12    movea.l (a2),a0
        jsr     (a0)
        dbra    d6,.t12
        bsr     tread
        move.l  d0,(a3)+                ; row 12

        bsr     fast1w
        tst.l   d0
        beq     .r13s
        movea.l d0,a0
        adda.l  #$1c000,a0              ; jsr target in fast region 1
        move.w  #$4e75,(a0)             ; rts
        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t13    jsr     (a0)
        dbra    d6,.t13
        bsr     tread
        move.l  d0,(a3)+                ; row 13
        bra     .r13d
.r13s   clr.l   (a3)+
.r13d

        ; --- row 14: movem push+pop, SP in fast1 (else chip) --------------
        bsr     fast1w
        tst.l   d0
        beq     .r14c
        move.l  sp,SAVSP
        movea.l d0,sp
        adda.l  #$18000,sp
        bsr     movemrow
        movea.l SAVSP,sp
        bra     .r14d
.r14c   bsr     movemrow
.r14d   move.l  d0,(a3)+                ; row 14

        ; --- row 15: cpushl line walk over fast1 (040/060 only) -----------
        cmp.w   #$40,CPUTYP
        blo     .r15s
        bsr     fast1w
        tst.l   d0
        beq     .r15s
        movea.l d0,a0
        clr.w   FAULTF
        lea     mskip2(pc),a1
        move.l  a1,VECTBL+$2c           ; F-line: single-word skip
        cnop    0,4
        bsr     tstart
        move.w  #ITER4K-1,d6
.t15    dc.w    $f468                   ; cpushl dc,(a0)
        lea     16(a0),a0
        dbra    d6,.t15
        bsr     tread
        lea     mskip4(pc),a1
        move.l  a1,VECTBL+$2c
        tst.w   FAULTF
        beq     .r15k
        moveq   #-1,d0                  ; faulted: emulator/CPU gap signal
.r15k   move.l  d0,(a3)+                ; row 15
        bra     .r15d
.r15s   clr.l   (a3)+
.r15d

        ; --- rows 16/17: chip window --------------------------------------
        movea.l #CHIPW,a0
        bsr     regread
        move.l  d0,(a3)+                ; row 16
        movea.l #CHIPW,a0
        bsr     regwrite
        move.l  d0,(a3)+                ; row 17

        ; --- rows 18-26: the fast regions, MemList order ------------------
        moveq   #0,d5                   ; region index
.rreg   bsr     regwin                  ; d0 = window for region d5, or 0
        tst.l   d0
        beq     .rregs
        move.l  d0,d4
        sub.l   #$10000,d0
        move.l  d0,(a3)+                ; base row (window - $10000 = lower)
        movea.l d4,a0
        bsr     regread
        move.l  d0,(a3)+                ; read row
        movea.l d4,a0
        bsr     regwrite
        move.l  d0,(a3)+                ; write row
        bra     .rregn
.rregs  clr.l   (a3)+
        clr.l   (a3)+
        clr.l   (a3)+
.rregn  addq.w  #1,d5
        cmp.w   #3,d5
        bne     .rreg

        ; --- rows 27-29: the composite page walk --------------------------
        ; One iteration per 4K page of fast region 1, replicating the traced
        ; mmu.library shape: register save/restore, two calls through a
        ; vector, a supervisor round trip, a page read, a descriptor write,
        ; and (27 only) a CPUSHL of the freshly written descriptor line.
        bsr     compbase                ; d0 = walk base, or 0
        tst.l   d0
        beq     .r27s
        cmp.w   #$40,CPUTYP
        blo     .r27s                   ; full row needs cpushl: 040/060
        movea.l d0,a0
        movea.l #DESCT,a1
        lea     vecblk(pc),a2
        bsr     fast1w                  ; the real walk's supervisor stack
        tst.l   d0                      ; sits in fast RAM: match it, and
        beq     .r27s                   ; skip when region 1 cannot host it
        move.l  sp,SAVSP
        movea.l d0,sp
        adda.l  #$18000,sp
        cnop    0,4
        bsr     tstart
        move.w  #PAGES-1,d6
.t27    movem.l d2-d7/a3,-(sp)
        movea.l (a2),a3
        jsr     (a3)
        movea.l 4(a2),a3
        jsr     (a3)
        trap    #0
        move.l  (a0),d2
        lsr.l   #8,d2
        move.l  d2,(a1)
        dc.w    $f469                   ; cpushl dc,(a1)
        addq.l  #4,a1
        lea     $1000(a0),a0
        movem.l (sp)+,d2-d7/a3
        dbra    d6,.t27
        bsr     tread
        movea.l SAVSP,sp
        move.l  d0,(a3)+                ; row 27
        bra     .r27d
.r27s   clr.l   (a3)+
.r27d

        bsr     compbase
        tst.l   d0
        beq     .r28s
        cmp.w   #$40,CPUTYP
        blo     .r28s
        movea.l d0,a0
        movea.l #DESCT,a1
        lea     vecblk(pc),a2
        bsr     fast1w                  ; the real walk's supervisor stack
        tst.l   d0                      ; sits in fast RAM: match it, and
        beq     .r28s                   ; skip when region 1 cannot host it
        move.l  sp,SAVSP
        movea.l d0,sp
        adda.l  #$18000,sp
        cnop    0,4
        bsr     tstart
        move.w  #PAGES-1,d6
.t28    movem.l d2-d7/a3,-(sp)
        movea.l (a2),a3
        jsr     (a3)
        movea.l 4(a2),a3
        jsr     (a3)
        move.l  (a0),d2
        lsr.l   #8,d2
        move.l  d2,(a1)
        dc.w    $f469                   ; cpushl dc,(a1)
        addq.l  #4,a1
        lea     $1000(a0),a0
        movem.l (sp)+,d2-d7/a3
        dbra    d6,.t28
        bsr     tread
        movea.l SAVSP,sp
        move.l  d0,(a3)+                ; row 28 (no trap)
        bra     .r28d
.r28s   clr.l   (a3)+
.r28d

        bsr     compbase
        tst.l   d0
        beq     .r29s
        movea.l d0,a0
        movea.l #DESCT,a1
        lea     vecblk(pc),a2
        bsr     fast1w                  ; the real walk's supervisor stack
        tst.l   d0                      ; sits in fast RAM: match it, and
        beq     .r29s                   ; skip when region 1 cannot host it
        move.l  sp,SAVSP
        movea.l d0,sp
        adda.l  #$18000,sp
        cnop    0,4
        bsr     tstart
        move.w  #PAGES-1,d6
.t29    movem.l d2-d7/a3,-(sp)
        movea.l (a2),a3
        jsr     (a3)
        movea.l 4(a2),a3
        jsr     (a3)
        trap    #0
        move.l  (a0),d2
        lsr.l   #8,d2
        move.l  d2,(a1)+
        lea     $1000(a0),a0
        movem.l (sp)+,d2-d7/a3
        dbra    d6,.t29
        bsr     tread
        movea.l SAVSP,sp
        move.l  d0,(a3)+                ; row 29 (no cpushl: every CPU)
        bra     .r29d
.r29s   clr.l   (a3)+
.r29d

        ; --- row 30: bare page step ---------------------------------------
        bsr     compbase
        tst.l   d0
        beq     .r30s
        movea.l d0,a0
        cnop    0,4
        bsr     tstart
        move.w  #PAGES-1,d6
.t30    tst.l   (a0)
        lea     $1000(a0),a0
        dbra    d6,.t30
        bsr     tread
        move.l  d0,(a3)+                ; row 30
        bra     .r30d
.r30s   clr.l   (a3)+
.r30d

        ; --- row 31: frame length -----------------------------------------
        bsr     syncframe
        bsr     tstart
        bsr     syncframe
        bsr     tread
        move.l  d0,(a3)+                ; row 31

        move.w  #$0ff0,$180(a6)         ; border yellow: all rows done
        bra     finish

;----------------------------------------------------- unsupported (68000)
unsupported:
        lea     RESULTS,a3
        move.l  #$acce1b02,(a3)+
        clr.l   (a3)+                   ; row 1 CPU = 0: unsupported
        moveq   #0,d0
        move.w  ATTNF,d0
        move.l  d0,(a3)+
        move.w  #(32-3)-1,d0
.uz     clr.l   (a3)+
        dbra    d0,.uz
        ; fall through: still render + stream what we have

;----------------------------------------------------- render + serial + show
finish:
        bsr     render
        move.w  #$0170,$032(a6)         ; SERPER ~9600 baud
        lea     RESULTS,a2
        moveq   #32-1,d4
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

        move.w  #$1000,$100(a6)         ; 1 bitplane lores
        move.w  #$0000,$102(a6)
        move.w  #$0000,$104(a6)
        move.w  #$0000,$108(a6)
        move.w  #$0038,$092(a6)
        move.w  #$00d0,$094(a6)
        move.w  #$2c81,$08e(a6)
        move.w  #$2cc1,$090(a6)
        move.w  #$0000,$180(a6)
        move.w  #$0fff,$182(a6)
        move.w  #$8300,$096(a6)         ; DMAEN | BPLEN
.show   bsr     syncframe
        move.l  #SCREEN,d0
        move.w  d0,$0e2(a6)
        swap    d0
        move.w  d0,$0e0(a6)
        bra     .show

;----------------------------------------------------- helpers ---------------
; Any unexpected exception: magenta border, halt. The row values already in
; RESULTS stay on screen after a re-run, so a fatal is visible and localised.
fatal:  move.w  #$0f0f,CUST+$180
.f      bra     .f

; TRAP #0: the measured supervisor round trip.
t0rte:  rte

; Skip a faulting 4-byte instruction (movec) / 2-byte instruction (cpushl).
; The stacked PC is at 2(sp) on every 68010+ frame format.
mskip4: move.w  #1,FAULTF
        addq.l  #4,2(sp)
        rte
mskip2: move.w  #1,FAULTF
        addq.l  #2,2(sp)
        rte

; near jsr/rts target and the two vector-called stubs of the composite
nearrts:
        rts
stub1:  rts
stub2:  moveq   #0,d0
        rts
        cnop    0,4
vecblk: dc.l    0               ; patched at startup: stub1, stub2 runtime
        dc.l    0               ; addresses (the binary is loaded at $30000)

; fast1w: d0 = fast region 1 window base (lower + $10000), or 0 if region 1
; is absent or smaller than $30000. Preserves all other registers.
fast1w: move.l  d1,-(sp)
        moveq   #0,d0
        tst.w   REGCNT
        beq     .f1d
        move.l  REGTBL+4,d1             ; upper
        sub.l   REGTBL,d1               ; - lower
        cmp.l   #$30000,d1
        blo     .f1d
        move.l  REGTBL,d0
        add.l   #$10000,d0
.f1d    move.l  (sp)+,d1
        rts

; regwin: d0 = window base of fast region d5 (0-2), or 0. Same guard.
regwin: move.l  d1,-(sp)
        move.l  a0,-(sp)
        moveq   #0,d0
        cmp.w   REGCNT,d5
        bhs     .rwd
        lea     REGTBL,a0
        move.w  d5,d1
        lsl.w   #3,d1
        adda.w  d1,a0
        move.l  4(a0),d1                ; upper
        sub.l   (a0),d1                 ; - lower
        cmp.l   #$30000,d1
        blo     .rwd
        move.l  (a0),d0
        add.l   #$10000,d0
.rwd    movea.l (sp)+,a0
        move.l  (sp)+,d1
        rts

; compbase: d0 = composite walk base in the LARGEST captured fast region
; (needs 2M of pages + slack past that region's scratch window), or 0.
; v1 walked region 1, which silently skipped the whole composite on a
; machine whose first fast region is the real A4000's 2M motherboard bank;
; the largest region is also the one whose walk cost dominates a real
; boot-time MMU table build. The composite's STACK stays in region 1
; (fast1w), matching the v1 060 column's stack placement.
compbase:
        move.l  d1,-(sp)
        moveq   #0,d0
        move.l  LARGHI,d1               ; largest fast region of the WHOLE
        sub.l   LARGLO,d1               ; MemList (tracked at capture time)
        cmp.l   #$220000,d1             ; window + 2M walk + slack
        blo     .cbd
        move.l  LARGLO,d0
        add.l   #$20000,d0              ; clear of that region's window
.cbd    move.l  (sp)+,d1
        rts

; regread/regwrite: stride-16 move.l over the 64K window at a0; d0 = ticks.
        cnop    0,4
regread:
        bsr     tstart
        move.w  #ITER4K-1,d6
.rr     move.l  (a0),d0
        lea     16(a0),a0
        dbra    d6,.rr
        bsr     tread
        rts
        cnop    0,4
regwrite:
        bsr     tstart
        move.w  #ITER4K-1,d6
.rw     move.l  d1,(a0)
        lea     16(a0),a0
        dbra    d6,.rw
        bsr     tread
        rts

; movemrow: the row-14 body (SP prepared by the caller); d0 = ticks.
        cnop    0,4
movemrow:
        bsr     tstart
        move.w  #ITER1K-1,d6
.mm     movem.l d0-d5/a0-a4,-(sp)
        movem.l (sp)+,d0-d5/a0-a4
        dbra    d6,.mm
        bsr     tread
        rts

;----------------------------------------------------- serial, beam, timer ---
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

; NTSC-safe frame sync (threshold 240 fits both 262- and 312-line frames).
; vpos is assembled from TWO register reads (V8 from VPOSR, V7-0 from
; VHPOSR); a pair straddling the line 255->256 boundary assembles a bogus 0,
; which a fast CPU's tight poll loop hits reliably. Debounce with two
; consecutive agreeing samples so the wrap detect only fires on the real
; frame wrap. Clobbers d0-d2.
syncframe:
.hi     bsr     getvpos2
        cmp.w   #240,d0
        blo     .hi
.wrap   bsr     getvpos2
        cmp.w   #240,d0
        bhs     .wrap
        rts
getvpos2:
        bsr     getvpos
        move.w  d0,d2
        bsr     getvpos
        cmp.w   d0,d2
        bne     getvpos2
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

;----------------------------------------------------- render 32 rows --------
render:
        lea     SCREEN,a1
        move.w  #40*256/4-1,d0
.rc     clr.l   (a1)+
        dbra    d0,.rc
        lea     RESULTS,a2
        moveq   #0,d4
.rr     move.l  (a2)+,d3
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
.rd     rol.l   #4,d3
        move.l  d3,d0
        bsr     .glyph
        dbra    d6,.rd
        addq.w  #1,d4
        cmp.w   #32,d4
        bne     .rr
        rts

.glyph: and.w   #$f,d0
        lsl.w   #3,d0
        lea     font(pc),a4
        adda.w  d0,a4
        move.l  a5,a1
        adda.w  d2,a1
        moveq   #6,d5
.rg     move.b  (a4)+,(a1)
        adda.w  #40,a1
        dbra    d5,.rg
        addq.w  #1,d2
        rts

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
