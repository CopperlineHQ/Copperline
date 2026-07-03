# 68000 cycle-timing gap vs SingleStepTests (MAME microcoded core)

Measured by `cycle_gap_report` in `tests/singlestep_m68000_v1_tests.rs`
(run: `cargo test --release --test singlestep_m68000_v1_tests cycle_gap_report
-- --ignored --nocapture`). Baseline = the vendored core as published (0.1.5),
before any timing fixes.

## Totals (261,894 cases, 127 opcode files)

- **Cycle mismatches: 48.3%** of cases.
- **Sum CPU cycles / fixture = 0.834** -- the core under-bills cycles ~17%
  overall (much worse for the instruction classes below). This is exactly why
  Copperline's cycle-accurate pacing runs the CPU too fast and over-issues chip-bus
  accesses, starving the blitter (TEK Rampage judder).
- Bus-access count mismatches: 19.4%; sum CPU accesses / fixture = 0.933 (the
  core does slightly *fewer* word accesses -- no prefetch discards near
  branches -- so the Copperline over-issue is driven by the cycle under-bill, not an
  access-count excess).

## Worst classes (systematic, not random)

- **`.l` (long) ALU/unary: 100% wrong.** ADD.l/SUB.l/CMP.l/AND.l/OR.l/EOR.l,
  NEG.l/NEGX.l/NOT.l/CLR.l/TST.l -- the core returns the same cycles as `.w`/`.b`
  (no extra cost for 32-bit operation). avg delta ~ -11..-14.
- **Long shifts/rotates: 100% wrong.** ASL/ASR/LSL/LSR/ROL/ROR `.l` off by -2
  (long base is +2 over word); ROXL/ROXR off much more (X path, count-dependent).
- **MULS/MULU: 100% wrong, avg ~ -21** (multiply timing is data-dependent on the
  multiplier's bit pattern; the core returns a flat value).
- **DIVU/DIVS: ~100% wrong, avg +75** (the only *over*-billing class).
- **Memory-operand variants** of MOVE/ALU/bit ops: high mismatch -- the
  Effective Address calculation cycle cost is not added.
- **Already correct (0% cycle mismatch):** branches (Bcc/BSR), RTS/RTR/RTE,
  TRAP/TRAPV/STOP/RESET, ORI/ANDI/EORI to CCR/SR, line-A/line-F illegal.

## Fix shape (Part D)

1. Size cost: `.l` operations add the documented extra cycles over `.w`.
2. EA-cost table: add Effective Address calculation cycles (by mode + size,
   Motorola M68000 User's Manual / "Yacht" tables) to memory-operand forms.
3. Shift/rotate: base + 2*count; fix ROXL/ROXR.
4. MUL/DIV: data-dependent timing (MULU/MULS bit-count; DIVU/DIVS algorithm).
5. Re-run the report; drive CPU/fixture cycle ratio toward 1.000.

Gate all 68000 timing changes by `CpuType::M68000` so 020/030/040 returns are
unaffected. The bus-access-count assertion (already wired) and a future prefetch
model tighten the secondary gap.

## Result after Part D timing work (2026-05-31)

Cycle mismatch across 261,894 cases: 48.3% -> 0.9%. Sum CPU/fixture cycle
ratio: 0.834 -> 0.999. All common/demo-relevant instructions match exactly.
Remaining: DIVS normal-path +/-2 in ~23% of cases (data-dependent loop, avg ~0
so no pacing bias), CHK trap-path timing, and a small static-register bit-op
edge. DIVU/MULU/MULS exact.

# Part E: prefetch + per-access bus timing (cycle-exact frontier)

Part D made per-instruction cycle TOTALS exact. What remains for true
cycle-exactness is the access stream itself: WHICH bus accesses happen
(prefetch model) and WHEN each lands within the instruction (sync timing).
Two further reports in `tests/singlestep_m68000_v1_tests.rs` measure this
against the fixtures' per-transaction logs:

- `access_sequence_gap_report` -- order/direction/address/data of accesses
- `access_timing_gap_report` -- per-access cycle offsets (only measured for
  cases whose sequence already matches)

## Phase 0 baseline (2026-06-03, post-Part-D core)

Sequence mismatches: **261,894 / 261,894 (100.0%)**, broken down:

- **by address: 200,351 (76.5%)** -- the prefetch signature. The core fetches
  the opcode at PC at execution time; real hardware already prefetched it, so
  the fixture's in-instruction fetches are the *overlap* prefetches at PC+4
  onward. (NOP: core reads PC, fixture reads PC+4. Both 1 access.)
- **by count: 50,865 (19.4%)** -- missing prefetch refills / discarded
  prefetches. 100% of flow-change instructions (Bcc taken, BSR, JMP, JSR,
  RTS/RTE/RTR, TRAP, STOP, ORI/ANDI/EORI to CCR/SR) mismatch by count: the
  real CPU refills the 2-word queue from the new PC.
- **by direction: 10,678 (4.1%)** -- access-order differences (e.g. the core's
  write_long is high-word-then-low-word; predecrement pushes on real hardware
  write low word first; PEA/TAS/ADDX.l/SUBX.l show this).

Per-access timing: not yet measurable (0 sequence-matching cases); becomes
the Part E.2 scoreboard once the prefetch model lands.

## Fix shape (Part E)

1. **E.1 prefetch queue** (IRC/IRD, Moira-style): opcode comes from the queue;
   extension words consume IRC; prefetch() refill at end of instruction;
   full_prefetch() on flow changes. Gated to CpuType::M68000.
   Target: sequence report ~0% mismatches; cycle report does not regress.
2. **E.2 sync() bus-timing callback**: `AddressBus::sync(cpu_clocks)` reports
   internal clocks before each access; instruction handlers ordered so every
   access lands at the fixture's exact cycle offset.
   Target: timing report 100% exact (known fixture anomalies excluded: the
   upstream README flags TAS's 5-cycle RMW timing and TRAPV as unreliable).

## E.1 progress (2026-06-03): 100% -> 18.1% sequence mismatch

Model implemented (all gated to CpuType::M68000):

- `prefetch_queue: [u16; 2]` + `prefetch_count` on CpuCore; the queue holds
  the words at pc/pc+2. Fixture harness preloads it from the fixtures'
  initial `prefetch` field.
- **Opcode consume** (`fetch_opcode`): from the queue, no bus access (the
  previous instruction's final prefetch supplied it).
- **Extension/immediate consume** (`read_imm_16/32`): from the queue, plus an
  immediate accompanying "np" fetch (`top_up_prefetch_one`) -- BEFORE the
  instruction's data accesses, matching hardware EA-calculation order.
- **End-of-instruction top-up** (`top_up_prefetch` in all three step
  variants): the final prefetch, after the instruction's writes (MOVE-class
  ordering). No-op when the queue is already full or the CPU is stopped.
- **RMW writeback** (`write_resolved_ea` memory arm): tops the queue up
  BEFORE the write -- RMW instructions prefetch before their writeback,
  unlike MOVE.
- **Flow changes** (`full_prefetch` / `prefetch_first`+`prefetch_second`):
  Bcc/BRA/DBcc taken, JMP, RTS/RTE/RTR refill from the target; JSR/BSR
  interleave (first prefetch, push, second prefetch). Their
  displacement/address words are consumed in `consume_without_prefetch`
  microcode mode (no np ahead of a stream about to be discarded).
- **Status ops** (ORI/ANDI/EORI to CCR/SR, MOVE to CCR/SR): refill after the
  status write. **STOP**: consumes its operand without prefetch, no top-up.
- **CLR**: reads its destination before writing (68000 quirk).
- `EaResult::Immediate` now carries the VALUE (consumed via the queue), not
  an address re-read later as data.

Results: sequence mismatches 18.1% (count 2.5%, direction 4.7%, address
10.9%); access-count ratio 0.993; cycle totals unchanged (0.9% / 0.999);
all 127 functional files + all 684 Copperline tests green; per-access timing now
measurable: 68.5% of accesses already land at fixture-exact offsets.

### E.1 iteration 2 (2026-06-03): 18.1% -> 1.5%

Fixed in this round:

- **68000 exception stacking order** (push_exception_frame_68000): PC low
  word, then SR, then PC high word -- cleared ILLEGAL_LINEA/LINEF, TRAP, CHK,
  RTE, all the to/from SR/CCR/USP ops, STOP, RESET, TRAPV-taken (the whole
  exception family, ~25k cases).
- **Long RMW writeback order**: low word then high word (write_resolved_ea)
  -- cleared NEG.l/NEGX.l/NOT.l/CLR.l/EOR.l/ADD.l/SUB.l/AND.l/OR.l.
- **LINK**: displacement consumed (with np) before the An push.
- **PEA**: final prefetch before the address push.
- **BSR**: push first, then the target refill (JSR interleaves, BSR does not).
- **Scc / MOVE from SR**: read-before-write quirk (like CLR), via
  resolve-once + read_resolved + write_resolved.
- **ABCD/SBCD -(Ax),-(Ay)**: final prefetch before the destination write.
- **TAS / TRAPV excluded** from the access-stream reports (upstream README
  documents both fixtures' transaction logs as unreliable); their final-state
  assertions still run in the functional suite.

### E.1 iteration 3 (2026-06-03): 1.5% -> 0.1%

Fixed in this round:

- **MOVEM memory-to-register**: the extra discarded word read past the last
  transferred register.
- **DBcc**: condition/counter evaluated before the displacement consume
  (branching paths consume without np); the counter-expired path reads one
  word at the abandoned branch target before refilling from fall-through.
- **ADDX.L/SUBX.L -(Ax),-(Ay)**: predecrement long reads go low word first;
  the writeback interleaves the final prefetch between the low and high
  result writes (read_long_predec_68000 / write_long_mm_interleaved_68000).
- **MOVE destination modes**: predecrement destinations prefetch before the
  write (and long predec writes descend: low word then high); (xxx).l
  destinations consume their address low word without its np and take both
  remaining prefetches after the write (Class 2).

### E.1 remaining tail (0.1% = 371 / 256,894 cases)

- **PEA (182; direction)**: index-mode (d8,An,Xn) np ordering.
- **MOVEM.l (157; address)**: long word order within register transfers
  (register-to-memory predecrement form).
- **MOVE.b/w/l (26; direction)**: residual mode combinations.
- **Bcc (6; count)**: edge cases.

Access-count mismatches are now 0.5% (ratio 1.000); cycle totals unchanged
(0.9% / 0.999). These remaining cases do not block Part E.2 (sync timing);
they can be cleaned up alongside it.

# Part E.2: sync() per-access timing

## Infrastructure (landed 2026-06-03)

- `AddressBus::sync(cpu_clocks)` default no-op trait method.
- `CpuCore::internal_cycles(clocks)` accumulates internal (non-bus) clocks
  into `pending_sync_clocks`; `flush_sync` reports them to the host
  immediately before every bus access (wired into read_8/16/32,
  write_8/16/32, prefetch_read). Gated to the 68000.
- RecordingBus in the harness captures sync values; the timing report
  computes each access's offset as sum(sync) + 4 clocks per preceding
  access and compares to the fixture transaction offsets.

With zero internal_cycles() calls placed, 53.1% of accesses are already at
exact offsets (instructions whose internal cycles all TRAIL their last
access -- trailing internals don't shift any access).

## E.2 worklist: classes needing internal_cycles() placement

From the timing report (avg |offset delta| in CPU clocks; the value
indicates where the internal period sits):

| Class | avg delta | likely placement |
|---|---|---|
| BSR, SBCD, ABCD, ADDX.b/.l, SUBX.b/.l | 2.00 | 2 leading internal clocks before the first access ("n" in Yacht) |
| Bcc taken | 2.92 | 2 internal clocks before the target refill ("n np np") |
| DBcc | 3.35 | 2-4 internal clocks before refill, per path |
| ILLEGAL/TRAP/STOP/RESET exception entries | 4.29 | internal cycles at exception start (vector compute) |
| MOVEtoSR/CCR, ORI/ANDI/EORItoSR/CCR | 4-8 | internal clocks before the post-status refill |
| CHK | 8.93 | comparison internals before exception/prefetch |
| RTE | 4.29 | internals between stack pops and refill |
| JSR/JMP | 3.85-3.93 | EA-calc internals (indexed modes) |
| DIVU | 40.35 | data-dependent division loop before accesses |
| DIVS | 53.15 | data-dependent division loop |
| RESET instruction | 19.39 | the 124-clock reset pulse |

Method (same as E.1): pick the class, dump fixture cases with
M68K_GAP_FILE, read the offsets to see where the internal clocks sit, add
internal_cycles(n) calls at those microcode points, re-run
access_timing_gap_report. The Yacht table (and the fixtures themselves)
give the per-instruction sequences. Hard invariant to keep: cycle totals
(cycle_gap_report) must not change -- internal_cycles only REPORTS timing,
it must not alter the returned cycle counts.

## E.2 placement results (2026-06-03): 53.1% -> 99.6% exact

Internal-cycle placements landed (all validated against fixture offsets):

- **EA calculation** (resolve_ea): predecrement +2 before the operand access
  (sources/RMW only -- MOVE destinations cancel it); indexed modes +2 BEFORE
  the extension-word fetch.
- **Branches**: Bcc/BRA/BSR 2 internal on taken paths / 4 on not-taken,
  before any bus activity; DBcc 4 when condition true / 2 on branch paths.
- **Memory-to-memory forms**: ABCD/SBCD/ADDX/SUBX 2 leading clocks (override
  of the per-EA charges for the double-predecrement forms).
- **Exception entry**: 4 leading clocks (take_exception) + 2 between the two
  handler-refill prefetches (jump_vector). CHK: 6 internal on pass, 8/10 on
  trap (too-big/negative). Status ops (to CCR/SR): 8 internal before the
  refill; MOVE to SR/CCR: 4.
- **Control flow EAs**: JMP/JSR +2 (d16/abs.w/PC-rel) or +4 (indexed) before
  the target refill; LEA/PEA indexed +2 after the extension fetch.
- **RESET instruction**: 128 internal clocks (the reset pulse).
- **DIVU/DIVS**: the data-dependent division clocks (divX_cycles - 4) before
  the final prefetch.

Final: 99.6% of accesses (99.7% of cases) at fixture-exact offsets.
The remaining 0.4% (CHK 388 cases, DIVS 353 cases, all avg-2-clock) is
bounded by the Part D cycle-TOTAL residuals in those classes (DIVS
data-dependent loop +/-2, CHK trap-path totals) -- placement cannot be more
exact than the totals it distributes. Improving those needs divs_cycles /
CHK-trap total formula fixes (Part D follow-up), not placement changes.

# Part E.3: Copperline integration + system validation (2026-06-03)

The host-side hard cutover landed in Copperline commit 2b66fd6 ("CPU
integration: consume cycle-exact per-access timing, delete the floor"):
CpuBus implements AddressBus::sync, chip/custom accesses pay a per-access
contended grant + bus-free tail, external accesses advance the chipset
bus-free, and all of the old approximation machinery (the 2*chip_accesses
floor, complete_cpu_slice_devices, slice_timed_* reconciliation,
pacing_budget modes) is deleted.

## System-level results (TEK Rampage vector scene, the acceptance test)

Measured with the BPL1PT flip log + copper-list patch probe over the
42-46.5s window (244 frames), Copperline pre-flip vs post-flip:

| Metric | pre-flip | post-flip |
|---|---|---|
| Torn-pointer noise frames | 0 (fixed by Part E.1 prefetch) | 0 |
| Backward-jump flicker | none (forward-monotonic) | none |
| Stale-repeat frames | 26.4% | 20.3% |
| Copper-list patch vpos (mode) | v30 | v29 |

The demo completes exactly one draw cycle per frame (1 clear + 1 fill +
N line blits started every frame); the residual stale repeats are a
margin problem: the patch lands ~3 lines before the copper's vpos-32
BPL1PT read, and per-frame jitter (varying line-blit count) occasionally
pushes it past. Demo cycle structure (per blit-sequence probe):
patch(v29) -> clear(v30-62) -> line blits + CPU gaps(v65-99) ->
fill(v99-284, racing the display region) -> CPU math tail(~28 lines,
gated by the demo's vblank wait) -> patch.

Cross-content regression (ITM, SOTA hand-crossing, Magic Pockets, EON,
Second Nature, SysInfo, KS1.3/KS2.05 boots): all render correctly.
Emulation throughput improved ~13% (the per-access model is cheaper than
the old reconciliation).

## Remaining gap (open)

- Blitter-yield miss-limit sensitivity is now ~zero (limits 2..6 give the
  same cycle time): starving the CPU during blits converts CPU slots into
  idle, not into earlier completion, because the CPU's overlapped compute
  is on the critical path. The HRM value 3 stays.
- Copperline's CPU takes ~18.4k chip-bus slots/frame in this scene vs a
  vAmiga reference measurement of ~17.9k (+2.7%). If that reference is
  accurate, ~500 slots/frame of CPU access excess remain unexplained
  (candidates: interrupt entry/IACK modelling, E-clock CIA access cost,
  per-PC access profiling needed). Closing it is the most likely path to
  eliminating the residual stale repeats.

## WinUAE cputest cycle validation (2026-07-03)

`crates/cputest-runner` now validates cycle counts against the cputest
data's CT_CYCLES records (`CPUTEST_CYCLES=1`; 68000/010 sets carry them).
Status after the fixes in this round (dynamic bit-op upper-half penalty,
BTST Dn,#imm, CHK source-EA cost, DIVS divisor-sign adjustment):

- **68000: cycle-exact across the full sweep** except JSR/JMP `(An)` with
  an odd address (the address-error path books 4 clocks differently -
  same exception-3 accounting class as the SST residuals above).
- **68010: not calibrated.** The sweep flags most instruction classes
  (the 010 has different EA/ALU costs, loop mode, and a faster MOVE from
  SR); the cputest data is the reference to do this against when 010
  timing matters.
