# CPU integration

## The wrapper and the bus adapter

`M68kMachine` (`src/cpu.rs`) wraps the published pure-Rust
[`m68k` 0.3 core](https://docs.rs/crate/m68k/0.3.0). The core sees
the machine through an adapter implementing its `AddressBus` trait, so
every CPU-visible access -- RAM, ROM, custom registers, CIA, RTC,
autoconfig, Gayle, Akiko -- routes into the shared `Bus` and is billed in
colour clocks:

- Chip and slow RAM go through chip-bus arbitration
  (`grant_cpu_bus_access`) and genuinely wait for free slots.
- Fast RAM, ROM, and other external-bus targets are billed at the CPU
  clock (`cpu_external_access`), scaled by `cpu_clocks_per_cck` with
  sub-CCK carry so accelerated clocks bill fractional costs exactly.
- Addresses are masked to the model's bus width: 24-bit for
  68000/68010/68EC020, 32-bit for 68020/030/040/060.

Selectable models: 68000, 68010, 68EC020, 68020, 68030, 68040, 68060.
`[cpu] fpu`
fits a 68881/68882 to any 020/030 (and is on by default for the 68040,
whose FPU is on-die): the `m68k` core executes the 6888x instruction
set in true 80-bit extended precision via a pure-Rust software floating-
point engine
([`softfloat.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/fpu/softfloat.rs)).
Arithmetic (add, sub,
mul, div, sqrt, and the single-accuracy FSGLMUL/FSGLDIV, which round the
mantissa to 24 bits but keep the extended exponent range -- gcc -m68040
emits them for `float` arithmetic and the Linux/m68k kernel FPSP has no
emulation entry for them, so a Line-F here is a userspace SIGILL),
ordered compare, round-to-integer, scale, getexp/getman,
the format conversions, FMOVE/FMOVEM in every operand format (including
packed decimal), the constant ROM, FBcc/FScc/FDBcc/FTRAPcc, control
registers, the FPCR rounding mode/precision and the FPSR exception/accrued
bytes, and the FSAVE/FRESTORE state frames (NULL after reset, and once
touched the CPU's own IDLE frame: the 68881-style $18-byte frame on
020/030 systems, the 68040's version-$41, $28-byte frame on the 040 --
guests validate the size byte, and Linux/m68k's sigreturn kills a process
whose saved FPU frame carries a foreign size) are all modelled. The transcendentals (FSIN/FCOS/FTAN,
FASIN/FACOS/FATAN, the hyperbolics, FETOX/FETOXM1/FTWOTOX/FTENTOX,
FLOGN/FLOGNP1/FLOG2/FLOG10) and FSINCOS run in extended precision too: a
double-`FloatX80` ("double-double", ~128-bit) layer
([`dd.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/fpu/dd.rs))
evaluates Taylor/atanh series over reduced
ranges and rounds the result to extended under the FPCR mode, setting INEX
and the domain flags (OPERR/DZ). Accuracy is validated against an
arbitrary-precision oracle (the pure-Rust `astro-float`, a dev-only
dependency;
[`fpu_accuracy.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/tests/fpu_accuracy.rs)):
every function is within
1 ULP across a wide sweep and all four rounding modes, and round-to-nearest
is correctly rounded in practice. They are not chip-bit-exact -- the real
6888x uses its own CORDIC/polynomial microcode, and on a bare 68040 these
trap to a software FPSP. FMOD/FREM compute the exact remainder and the FPSR
quotient byte. This covers Kickstart's
detection and per-task FPU context switching. The
68000's per-instruction cycle counts in the `m68k` core have been
corrected to exact totals across the SingleStepTests 68000 cycle corpus
([validation details](https://github.com/benletchford/m68k-rs/tree/m68k-v0.3.1#validation--testing)),
which is what makes
cycle-budgeted pacing trustworthy.

## The batch/JIT execution mode

`[cpu] jit` (experimental, 68020+) swaps the per-instruction precise loop
for the core's instruction-budgeted batch path
(`CpuCore::run_batch`): hot straight-line runs and self-loops execute as
compiled Cranelift traces (the `cpu-jit` cargo feature; without it, wasm
builds included, the same trace machinery runs interpreted), and the
largest fast-RAM bank is offered to the core as a zero-cost direct-memory
window (`AddressBus::fast_mem`). The timing contract is deliberately
approximate (`M68kMachine::step_slice_jit`):

- every retired instruction is billed a flat single CPU clock, converted
  to colour clocks at the configured clock ratio, and fast-RAM/ROM
  accesses bill nothing (an ideal zero-wait external bus) -- the machine
  behaves like a perfect pipelined accelerator whose throughput tracks
  `[cpu] clock_mhz` directly, e.g. ~50 MIPS for a 50 MHz 68040 in fast
  RAM. This is the deterministic equivalent of a "fastest possible"
  mode: a truly host-speed CPU would make emulated results depend on the
  host, which Copperline never allows;
- chip and slow RAM accesses still arbitrate into DMA slots through the
  normal `CpuBus` paths and advance the beam as they land (that bus is
  shared silicon), and CIA/RTC accesses keep their E-clock costs, keeping
  chipset side effects ordered against display DMA;
- interrupts are recognized only at batch boundaries (64 instructions),
  from the live INTENA/INTREQ state; a level the boundary cannot deliver
  is never left in the core, since the batch would take it as soon as the
  mask drops without recomputing INTREQ (a spurious handler re-entry);
- a slice that runs and then executes STOP is billed only its executed
  time -- the idle period is the next slice's event-bounded fast-forward,
  exactly like the precise path's STOP handling;
- traps surfaced by the batch (A-line/F-line/TRAP/BKPT/illegal) are taken
  as their real hardware exceptions, mirroring the precise loop's no-op
  HLE policy.

The on-chip cache models stay active under JIT: on a real accelerator it
is exactly the caches that let chip-RAM-resident code run at CPU speed
instead of paying chip-bus arbitration on every fetch, so bypassing them
made a fast-RAM-less Workbench measure 3x slower under JIT than precise.
A cache hit serves the access with no bus cycle; a miss pays the real
chip-bus (or zero-wait external) cost. CACR writes inside a batch are
synced to the models at the batch boundary.

The fastmem window is only offered when nothing intercepts plain fast-RAM
accesses: cache models, memory-write debug hooks, the heat map, SMC
detection, and injected bus faults all force every access back onto the
bus (so on default 020+ configs, whose caches are modelled, the window
engages only when the user opts the caches off -- and the core also
declines it whenever the guest enables the MMU, since fastmem addresses
are physical). Arming any per-instruction debug or diagnostic hook
(breakpoints, watches, traces, `COPPERLINE_DBG_*`) drops the whole slice
back to the precise loop, so the debugger always sees every instruction.

The 68000 and 68010 never take the batch path. On the small-box machines
every CPU cycle drives the one bus shared with Agnus, and the floating-bus
model is prefetch-order dependent: Kickstart's diagnostic-ROM probe
(`CMPI.W #$1111,(A1)` against unmapped `$F00000`) relies on the prefetch
queue having fetched the following words so the undriven bus does NOT
float to the immediate just read. The batch contract runs without
prefetch, which lands the float on the immediate and false-detects a
diagnostic ROM. `[cpu] jit` on these models logs a note and stays
precise.

## Prefetch

The 68000's two-word instruction prefetch queue (IRD/IRC) is modelled in
the `m68k` core
([`prefetch_queue`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/core/cpu.rs)):
the
next opcode is fetched before the current instruction finishes, so
self-modifying code that overwrites the *next* instruction executes the
stale pre-write word (real MC68000 Class 1 SMC behaviour), while a taken
branch flushes and refills the stream from the target. The queue lives in
the backend core rather than a Copperline-side bus cache, because correct
flushing depends on the CPU's own control flow, exceptions, and
interrupts. It is gated to the 68000 and 68010 (`prefetch_enabled`);
68020+ fetch directly at PC through the bus adapter (their real pipelines
hide behind the instruction cache model instead). Chip-RAM probes pin both
cases:
`cpu_prefetch_probe_documents_self_modified_next_opcode_behavior` (stale
fall-through) and
`cpu_prefetch_probe_branch_refetches_self_modified_chip_ram_target`
(branch refetch).

Instruction handlers can move the final prefetch earlier than the generic
end-of-instruction top-up when 68000 microcode does so. That matters for
register-only forms too: immediate ALU and CMPI to `Dn` perform the final
prefetch before the data-register write or compare flags, and long register
forms spend their trailing internal clocks after that prefetch. Plain
`CMP.L <ea>,Dn` computes its flags before the final prefetch, then spends its
2-clock long-compare tail after that prefetch. Long `ADD/SUB/AND/OR <ea>,Dn`
forms write `Dn` before the final prefetch, then spend their 2- or 4-clock
long-ALU tail after it depending on whether the source operand came from memory.
`ADDA/SUBA` and `CMPA` likewise place their address-arithmetic tail clocks after
the final prefetch. `EOR Dn,Dm` follows the memory-destination EOR ordering:
flags are computed first, then the final prefetch/tail runs before the `Dm`
writeback. Register `ADDX/SUBX Dm,Dn` also poll IPL on the final prefetch before
writing `Dn`, with long forms flushing their 4-clock tail first. `MOVE SR,Dn`
also waits until after the final prefetch and 2-clock tail before storing the SR
word into `Dn`. `MOVEA <ea>,An` has no flags or tail clocks, but still delays
the `An` update until after the final prefetch and IPL sample. The privileged
`MOVE An,USP` and `MOVE USP,An` forms use the same
prefetch-before-register-update point. Status-register writes also have
instruction-specific side-effect timing: `MOVE <ea>,CCR/SR` and
`ORI/ANDI/EORI #imm,SR` spend their internal status-write delay before the
architectural CCR/SR mutation and post-write refill, while immediate-to-CCR
mutates the CCR before that delay on the 68000.
`MOVEM.L <regs>,-(An)` also exposes the 68000's word-step predecrement
microcode on the bus: each long transfer writes the low word at `An-2`
before the high word at `An-4`, leaving memory big-endian while matching
the observed access order.
For `Bcc`/`BRA`/`BSR`, the branch-long `$FF` displacement-byte sentinel is
gated to 68020 and later; on the 68000/010 the same byte remains the signed
8-bit displacement `-1`, so no extension word is consumed.
`CHK.W` on the 68000 tests the upper bound before the lower bound; upper-bound
traps take the shorter pre-frame comparison path. A negative `Dn` that reaches
the lower-bound test also takes that shorter path when the preceding signed
upper-bound subtraction overflowed; ordinary lower-bound traps spend two more
clocks before stacking the group-2 exception frame.

## 68010

The 68010 shares the 68000's bus interface and two-word prefetch queue but
adds the vector base register, the format-stacking exception model
(format 0 four-word frames, format 8 bus/address-error frames, RTD), and
DBcc loop mode: a DBcc that branches -4 back to a loopable one-word
instruction holds the body/DBcc pair in the prefetch queue and re-executes
it with no instruction fetches until the condition turns true, the counter
expires, or an exception intervenes (`loop_mode` in
[`core/cpu.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/core/cpu.rs);
the loopable set and the DBcc entry/exit arms live in
[`core/decode.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/core/decode.rs)).
A looping DBcc iteration costs 6 internal
clocks and touches the bus only for the body's operands, which is what
makes tight copy/clear loops measurably faster on a real 68010.
[`loop_mode_timing_tests.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/tests/loop_mode_timing_tests.rs)
pins engagement, the
68000's non-engagement, and the no-fetch iteration cost.

The 68010's own cycle costs where they differ from the 68000 are
calibrated against the vAmigaTS `CPU/Timing`/`CPU/Timing2` measurements
(cross-checked with Moira's cycle-exact 68010 path, which matches
A500+68010 photos): MOVES spends per-EA-mode internal clocks between the
address calculation and the SFC/DFC data cycle, MOVE from CCR and
privileged MOVE from SR both cost 4 clocks to a register and perform their
final prefetch before updating `Dn`, MOVE from CCR to memory prefetches
before its write, long register shifts/rotates use the same base-8 cycle
total as the 68000 rather than the later barrel-shifter timing, a
format-0 RTE is 24 clocks (the format word is read once, not re-read),
and an interrupt dispatch is 46 clocks (12 internal before the four-word
format-0 frame) against the 68000's 44. STOP semantics shared with the
68000: the SR operand is loaded VERBATIM (a single-stepped STOP observes
S and T exactly as written; the SST m68000 fixtures pin this), and an
S-clear SR stops only momentarily -- at the next instruction boundary the
stopped state's supervisor check raises a privilege violation stacking
the STOP itself, so the handler's RTE re-executes it; a pending trace
(T set in the SR the instruction started with) has priority and recovers
from the stop, while a T bit loaded *by* STOP does not fire while
stopped.
[`stop_and_68010_timing_tests.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/tests/stop_and_68010_timing_tests.rs)
pins all of these.

## Caches

The on-chip caches are silicon, so they are modelled by default on the parts
that have them: the instruction cache on the 68020/68EC020/68030/68040 and the
data cache on the 68030/68040 (`CpuModel::has_instruction_cache`/
`has_data_cache`). CACR (and CAAR on the 020/030) are always stored; software
(AmigaOS at boot) enables and clears the cache exactly as on hardware. A cache
hit costs no bus cycle, so a cached instruction fetch does not contend with
chip-bus DMA -- which is the point: 020/030/040 code looping out of chip RAM
otherwise pays a bitplane-DMA arbitration stall on every fetch and runs at
roughly half speed, drifting an AGA demo's interrupt-driven music or animation
to half its intended rate. The data cache only covers expansion RAM/ROM
because chip and slow RAM are DMA-visible and cache-inhibited, as on real
machines. A 68000/68010 models no cache. `[cpu] icache = false`/`dcache =
false` opt a cached CPU back out; with no cache modelled, the cache-control
instructions are no-ops and self-modifying code always executes fresh bytes
(the safe direction).

Each cache is a power-of-two array of direct-mapped longword entries
(`src/cache.rs`): 64 entries (256 bytes) on the 020/030 -- the exact 68020
instruction-cache geometry -- and 1024 (4 KB) on the 68040. The larger 040
capacity is the part that matters here: a chip-RAM loop bigger than 256 bytes
stays resident on a 040 where it would thrash a 020. The 040's 4-way
set-associative, 16-byte-line, copyback organisation is deliberately not
modelled, and need not be -- copyback is unobservable because the data cache
only covers expansion RAM, which is not DMA-visible, so write-back versus
write-through cannot be told apart. The 040 also redefines CACR (only the IE/DE
enable bits, no freeze or clear strobes) and moves invalidation to the
CINV/CPUSH instructions; the model maps a CINV/CPUSH to a whole-cache clear of
the indicated cache(s) -- over-clearing line/page scopes, which is always
coherent (a push has no dirty data to write back in a write-through model).

The instruction cache does not snoop writes (authentic 68020 behaviour), so a
line stays valid until DMA or the CPU rewrites its backing memory, or software
clears it (via CACR on the 020/030, via CINV/CPUSH on the 040). Restoring a
save state that predates cache modelling (or was captured with the cache
disabled) re-establishes the model's cache cold and re-derives its enable bits
from the restored CACR, so the machine keeps the cache its CPU has rather than
silently running cacheless after a load.

## The 68060

The 68060 is modelled as the full part: on-die FPU and MMU, the 8 KB
caches (in the same pragmatic direct-mapped host model as the other
parts), and its own timing engine.

**Instruction set.** The 060 dropped instructions from silicon; on real
boards the OS-side `68060.library` (Motorola's 68060SP) emulates them
via the chip's trap interface, and Copperline models exactly that
contract. The unimplemented integer set (MOVEP, CHK2/CMP2, CAS2,
misaligned CAS, 64-bit MUL/DIV) raises vector 61 with a format $0 frame
stacking the faulting instruction, gated before any architectural side
effect so the handler can re-decode and re-execute. The unimplemented
FPU set (the transcendentals, FINT/FINTRZ, FGETEXP/FGETMAN,
FMOD/FREM/FSCALE, FSINCOS, FMOVECR, and the FDBcc/FTRAPcc/FScc
conditionals) raises the Line-F vector with the six-word format $2
frame the 68060SP dispatches on - next-instruction PC stacked, the
calculated operand EA in the frame, FPIAR pointing at the instruction.
Packed decimal is the unsupported-data-type exception (vector 55),
dynamic-list FMOVEM and immediate packed operands the unimplemented-EA
exception (vector 60). `[cpu] unimplemented = "native"` executes the
whole set directly instead, for systems without the library. LPSTOP is
implemented as STOP semantics.

**Registers and frames.** PCR (MOVEC $808) carries the identification/
revision word with EDEBUG/DFP/ESS writable; BUSCR shares MOVEC code
$008 with the 040's DACR0, dispatched by model. The 060 CACR persists
the cache/branch-cache/store-buffer enables with CABC/CUBC as
write-only clear strobes. The part has a single supervisor stack: the
SR M bit is storable (and interrupts clear it) but never banks A7, and
MSP/ISP are gone from the MOVEC set. Access errors push the eight-word
format $4 frame with the fault address and FSLW (composed from the MMU
walk cause, R/W, size, and transfer modifier), and RTE pops it - along
with the 040's format 7, which previously format-errored. The FPU state
frame is one long word for NULL (as every part since the 68881; exec's
hand-built task contexts depend on it) and three for IDLE. The MMU
shares the 040's three-level walker and TC[15] enable; PTEST is gone
(undefined F-line) and PLPAR/PLPAW translate an address into An,
faulting with the format $4 frame.

**Timing.**
[`timing_060.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/core/timing_060.rs)
replaces the 020+
scaling formula for the 060: every opcode word classifies (a build-once
64K table over a pure function) into the MC68060UM Chapter 10 dispatch
classes with a pOEP cycle cost - most ALU/move instructions one clock,
data-dependent costs derived from the 68000-reference count as
(raw/4).max(1). Costs are pOEP occupancy at zero-wait: memory latency
stays billed by the host bus per access, so bus-bound code shows no
pairing benefit, which matches silicon. Superscalar dual issue is
modelled retrospectively: an instruction that satisfies the UM Table
10-1 dispatch test against the previous instruction's open sOEP slot
(pOEP|sOEP class, simple EA, no RAW/WAW against the head's defs, CCR
rule, one memory operand per pair, both fetches icache hits, PCR.ESS
enabled) refunds to zero additional cycles. The 256-entry branch cache
(CACR.EBC; CABC/CUBC clears) folds correctly predicted taken branches
onto the preceding instruction's cycle - a lone predicted branch still
issues for one clock, which both matches the canonical one-clock
subq/bne loop and guarantees a bare `bra self` idle loop advances
emulated time. Both structures are serialized: prediction state changes
cycle counts, and cycle counts change chipset interleaving. timing-test
rows 28-30 measure the pair/RAW/loop cases end to end (a 50 MHz 060
runs row 30 at exactly one clock per iteration).

Residuals, all deliberately pessimistic (they under-pair, never
over-pair): the sOEP EA subset and dual-memory rule are coarser than UM
Table 10-1; the 96-byte IFU FIFO and AGU change/use stalls are not
modelled; the store buffer is stored (CACR.ESB) but writes bill at bus
rate; pairing across a folded branch is not modelled, and a branch
folds only when a pairing window is open; the FSAVE EXCP frame is not
generated (the softfloat FPU retires every operation completely); FPSR
quotient-bit and denormal traps behave as on the other parts rather
than through the 060's unsupported-data path.

## 68020 timing

The 68000/68010 cycle counts are validated against the
[SingleStepTests](https://github.com/SingleStepTests) (TomHarte) corpus,
but no equivalent vectors exist for the 68020. The later part has an
instruction cache, a three-stage pipeline, execution overlap, dynamic bus
sizing, and alignment-dependent transfers, so an opcode does not have one
context-free cycle count.

[`timing_020.rs`](https://github.com/benletchford/m68k-rs/blob/m68k-v0.3.1/src/core/timing_020.rs)
transcribes the integer timing tables
from section 8.2 of the
[MC68020 User's Manual](https://www.nxp.com/docs/en/data-sheet/MC68020UM.pdf).
The manual publishes Best Case (cached with maximum execution overlap),
Cache Case (cached without overlap), and Worst Case (uncached without
overlap). Copperline does not yet model execution-stage overlap, so an
instruction selects Cache Case only when all of its opcode, extension, and
immediate fetches hit the instruction cache; any instruction-stream miss
selects Worst Case. This covers the standard effective-address tables, the
complete MOVE matrix, arithmetic and logical instructions, multiply/divide,
shifts, bit/bitfield operations, branches, control flow, and save/restore
paths. The 68000 and 68010 timing paths are unchanged, and the 68030/040
retain the earlier scaled approximation rather than incorrectly inheriting
68020 silicon timings.

The manual's totals assume aligned operands and stack, a 32-bit bus, and
three-clock no-wait reads and writes. They include those ideal transfers.
Copperline bills every actual operand and instruction access while the
instruction executes, then advances only the unconsumed part of the table
total. An A1200 chip-RAM access can therefore extend an instruction through
Alice arbitration; selecting a smaller CPU total cannot erase a bus wait
that already happened.

A taken branch pays a pipeline-refill charge on top of its table entry. The
section-8.2 entries are written for an instruction whose successor is already
in the three-stage pipeline; a taken branch invalidates the decode and execute
stages, so the target cannot begin until the pipe refills. The manual charges
that to the *following* instruction's head, which a per-instruction model with
no overlap stage has nowhere to put, so the m68k core charges it where the
flush happens (`TAKEN_BRANCH_REFILL`). Without it a `dbra` loop runs at the
isolated cache-case 6 clocks per iteration, which makes every tight loop --
depackers, MFM decoders, chase-the-beam poll loops -- fast; it showed up
across seven independent `timing-test` rows (4, 5, 7, 14, 28, 29, 30). The
charge was first calibrated to two clocks against the FS-UAE A1200 reference;
a real A1200 (stock 68EC020 at 14.19 MHz, 2026-08) measures a cached taken
`dbra` at 7 clocks -- cache-case 6 plus one -- in three independent loop rows,
with the `move`, shift, `mulu`, paired-op and DIV-under-display rows agreeing
once that one clock is accounted for. The refill is therefore one clock, and
FS-UAE itself over-bills every taken branch by one; see
`timing-test/README.md`.

This is intentionally a datasheet model, not a claim of cycle exactness.
The opcode word does not retain a consumed extension word, so full-format
indexed modes use the brief-index table row, `MOVES` uses the slower of its
two direction bases, and long divide uses the conservative signed base.
Coprocessor operations and unclassified encodings retain the old scaled
fallback. Misalignment, detailed cache-refill sequencing, and instruction
overlap remain to be added. Motorola explicitly demonstrates in section 8.1
that the timing of an instruction stream cannot be obtained by simply
summing isolated table entries; hardware traces are still the final reference
once a machine is available. `timing-test/` remains the end-to-end regression
for checking the combined CPU, cache, and chip-bus model rather than the
source of the per-opcode constants.

On the A1200 the 020 uses a 32-bit Alice chip-bus data path and a two-entry
longword fetch latch. The 020's chip-bus cycle is modelled as 3
CPU clocks, not the 68000's 4: after the granted colour-clock slot the access
bills only the shorter remaining tail (one clock -- half a cck at the stock
2-clock ratio, none at 14 MHz where the 3-clock cycle fits inside one slot).
That is enough for a posted write. A chip-RAM read or custom-register read
also waits one colour clock for the chipset's data-return phase, and a
custom-register read waits one colour clock beyond that: chip RAM answers out of
Agnus' DRAM controller with the row already open for the granted slot, while a
register read has to cross to the addressed chip, be driven back onto the 16-bit
chipset bus, and only then meet the CPU's data-return phase. A 68000 cannot see
the difference -- its four-clock bus cycle is longer than either path -- but a
14 MHz 020 samples early enough that the crossing costs a whole slot. This is
what sets the rate of a VHPOSR/INTREQR polling loop, so it decides where
chase-the-beam code lands relative to the copper: `timing-test` rows 16, 17 and
21 fit 27% more iterations per frame without it. An AGA
instruction-cache fill does the same for its first word; the second word from
the already-latched 32-bit value is free. On OCS/ECS machines the 020 still
talks to the 16-bit chip bus, so chip/slow/custom data reads likewise pay the
return wait. The tail's fractional cck are carried so none are lost; the
68000/010 keep the full 4-clock (2-cck) cycle
(`Bus::cpu_short_bus_cycle`).

This distinction is visible in SysInfo 4.4: treating Alice reads and cache
fills like posted writes made the A1200 profile report 5.52x A600 chip speed
and 1.61x A1200 CPU speed after the instruction-table update. Billing the
data-return phase changes those figures to 3.55x and 1.10x respectively,
without altering register-only timing-test rows.

The real-A1200 column drove two further sub-cck mechanisms, both gated to the
020+ short-cycle path (the 68000's four-clock cycle spans the whole port
sequence, so it cannot see either).

Chip writes are posted. The 020's bus unit runs decoupled from its execution
unit: a chip write is accepted at the end of its 3-clock cycle and the
transfer overlaps the following instructions, retiring into a later free chip
slot. Copperline models this by crediting the transfer's clocks back against
the instruction's charge (`take_cpu_bus_overlap_clocks`) and letting the
pending write drain into the next free slot during subsequent beam
advancement (`cpu_posted_write_debt`), at the port's 2-cck turnaround
cadence. Only one bus cycle can be in flight, so a second chip access stalls
until the pending write retires -- which is what paces a chip write+dbra loop
to ~8 clocks per iteration on the real machine (`timing-test` rows 3, 10,
12, 18) where a synchronous whole-slot bill gave 11-12. A DMA reader of the
same address inside the drain window (at most two colour clocks) would see
the write early; that race is unobservable in practice and accepted.

Reads pay one further CPU clock synchronizing the returned data back into
the CPU clock domain, accumulated into whole colour clocks
(`bill_020_read_return_sync_clock`). The real chip-read loop measures 16.1
clocks per iteration (row 2) where grant plus data-return bill 15; the old
exact match on that row was the branch over-bill cancelling this missing
clock. The same synchronizer clock on custom-register reads is what lands
the copper-vs-CPU poll row (row 27) exactly on the real machine's beam
position.

The same column also pinned down 020 result forwarding, modelled in the m68k
crate: a register-to-register MOVE whose source is the register the
immediately preceding register-to-register MOVE wrote runs one clock faster
(the value is still in the execution unit's result latch, skipping the
register-file read). The real machine runs the RAW-dependent pair one clock
per iteration faster than the independent pair (`timing-test` rows 29 and
28: 10 against 11 clocks including the loop dbra), which only a forwarding
path can produce; wider forwarding is deliberately not modelled without
hardware data.

What remains against real silicon, with the middle rows now read off the
CRT across two runs: the composite write-plus-poll rows (16, 17, 21) run
3-7% under the real readings, and the raw interrupt-phase rows (19, 20, 22)
sit a few colour clocks early of readings that themselves move a few clocks
between real runs. Both are composite effects (posted-write drain against
the poll read, interrupt entry against the beam) rather than single-constant
gaps. The SysInfo figures in the previous paragraph predate posted writes
and will read higher on the A1200 profile.

## MMU

The 68030, 68040, and 68060 model the on-chip MMU (`has_pmmu`), so software that sets up
and enables address translation runs rather than having its MMU writes ignored.
The two parts differ enough to need separate walkers: the 68030 uses its
programmable table walk (CRP/SRP selection, the optional function-code lookup
level (TC FCL), the TC index fields TIA-TID, 4-/8-byte descriptor modes,
early-termination descriptors, indirect descriptors when the configured
levels are exhausted, and the long-format supervisor-only bit), while the
68040 has a fixed-format three-level walk (root -> pointer -> page, 4 KB or
8 KB pages, URP/SRP split by supervisor) keyed off its own TC layout. Both
share the transparent translation registers (TTRs: the 030's TT0/1, the
040's ITT0/1 + DTT0/1), which short-circuit the walk for a matching address
range. Accesses carry their function code into the walk: `MOVES` translates
in the SFC/DFC space (mmu.library's 030 MMU-detection probe remaps the
user-data space and reads it back with `MOVES` from supervisor mode), and
`PTEST`/`PLPA` probe the DFC space.

One register set (`mmu_*` in `CpuCore`) is canonical for both paths, so the 030
`PMOVE` writes, the 040 `MOVEC` writes, and the walker can never desync (the 040
root pointers overload the CRP/SRP address slots, dispatched by `cpu_type`). The
enable bit differs by part -- TC[31] on the 030, TC[15] on the 040 -- via
`tc_enable()`. `PMOVE`/`MOVEC` load the registers (including the 030 MMUSR
via `PMOVE PSR`); `PLOAD`/`PFLUSH` are accepted as no-ops on the 030 walk
(which has no ATC to flush).

A 68040 table walk costs three descriptor fetches, far too much to pay on every
access, so the 040 walker is fronted by an address-translation cache
(`crate::mmu::atc`, mirrored on the real 64-entry ATC): a direct-mapped cache of
(logical page -> physical page) keyed by page frame and supervisor flag. It is a
pure cache -- never serialized (restored empty) -- and is flushed when the
mapping could change: a write to TC / a root pointer / a TTR, or a PFLUSH. Like
real hardware a plain write to a descriptor does not auto-flush; software must
PFLUSH, which is where the ATC is cleared.

The 68040 enforces page protection: a write to a write-protected page (the W
bit, accumulated across the table and page descriptors) or a user access to a
supervisor-only page (the S bit) raises an access fault, delivered as a 68040
format-7 access-error stack frame at the bus-error vector. The faulting
instruction is rolled back and its PC stacked, so after the handler fixes the
mapping an `RTE` restarts it (the demand-paging / memory-protection model). The
ATC carries the protection bits, so a cached translation cannot bypass the
check. The frame's SSW reports the read/write direction, the access size, the
transfer modifier (function code), and -- for faults that came out of the
table walk rather than the physical bus -- the ATC bit; that bit is how an
OS-level page-fault handler (mmu.library, VMM, Enforcer) tells a translation
fault it must service from a real bus error it must pass on, and mmu.library's
lazily-materialized user tables guru without it (issue #90). A faulted write
is reported in writeback slot 3 (WB3S valid bit, size and transfer modifier,
address and data in WB3A/WB3D), matching real 68040 silicon -- WB2 is
reserved for MOVE16 cacheline writes and stays clear. A handler that clears
WB3S.V has absorbed the store -- how Enforcer/MuForce discard writes into
protected pages -- and RTE honours that by discarding the restarted
instruction's matching write; MuGuardianAngel completes an allowed write by
storing WB3D to WB3A itself. A handler that completes the writeback manually
but leaves V set gets the restart's store instead (a double store to plain
memory, the restart-model gap). The other writeback slots and continuation
fields stay clear; mid-instruction continuation is not modelled.

Any access through an invalid/unconfigured descriptor raises an access
fault, instruction fetches included -- data faults are how Enforcer/MuForce
catch low-memory and freed-memory hits, and fetch faults are how a
demand-paged OS (Linux/m68k) pages code in; software that must keep
executing across an MMU enable covers itself with the transparent
translation registers. The fault delivery itself translates like any other
supervisor access: the frame pushes and the vector fetch go through the
MMU (supervisor stacks and VBR hold logical addresses -- Linux runs its
kernel at virtual 0 with RAM at a physical offset and splits URP from
SRP, so an untranslated push or vector read lands in unrelated physical
memory), a `MOVES` fault's SFC/DFC override is consumed into the frame's
SSW rather than leaking into the dispatch, and a fault raised while
delivering a fault is a clean double-fault halt. Because the rollback
model re-executes the faulting instruction, its post-fault side effects
are also contained: bus accesses are suppressed, and the handler-entry
PC/SR/register state is re-asserted at the instruction boundary so an
aborted flow instruction (a `JSR` whose stack push faulted) cannot divert
the dispatch into its branch target, nor a predecrement `MOVEM` walk the
handler's stack pointer away.

Access granularity follows the 32-bit bus: an aligned long transfer is one
bus cycle, so a faulted long RMW writeback is reported as one long-sized
WB3 entry (not the 68000's low-word-first word pair -- a writeback-
completing handler like Linux `do_040writebacks` would otherwise complete
half a store). A misaligned access that straddles an MMU page boundary
runs as separate cycles with each side translated on its own page, data
and instruction-stream fetches alike; translating only the base address
would touch the physically adjacent page instead of the mapped one (under
Linux/m68k that mis-fetched the low half of a page-straddling `BSR.L`
displacement and sent execution mid-instruction).

`PTEST` (68040) walks the addressed page and reports the physical address and
resident bit in MMUSR (the cache-mode/used/modified attribute bits are not yet
filled in). The 68030 `PTEST` performs a real level-limited walk in the
extension word's function-code space and composes the 16-bit MMUSR (B, S, W
-- reported for read tests too -- I, T, and the level count N); with the A
bit it hands back the physical address of the last descriptor examined,
which is how mmu.library's fault handler locates the shared descriptor slot
to materialize.

68030 faults push the long bus-cycle fault frame (format $B, 46 words):
SSW (DF/RW/SIZ/FC for data faults, FB|RB with the stage B address for
instruction faults), the data-cycle fault address, and -- for write faults
-- the write's value in the data output buffer. `RTE` supports both
continuation protocols on top of the rollback/restart core: a handler that
fixed the mapping RTEs with DF set and the instruction restarts; a handler
that *completed* the data cycle itself clears DF, supplying a faulted
read's result in the data input buffer (mmu.library emulates lazily-zeroed
pages this way) or absorbing a faulted write, honoured as a one-shot
substitution on the re-executed instruction's matching access. Remaining:
the used/modified/limit MMUSR bits and descriptor write-back, CRP limit
checks, and the DT=1 page-descriptor root; the 030 walk has no ATC, so
every access re-walks the tables.

## Interrupts and STOP

Paula's INTENA/INTREQ levels are delivered as M68K autovectors through the
modelled IPL pipe and boundary sampling described in [](timing). When the CPU
executes `STOP`, the frame loop fast-forwards device time to the next
event that can raise an interrupt instead of spinning -- behaviour the
debugger's Step control inherits. One event source is invisible to that
horizon: a serial sink with a live host input side (TCP, pty, the browser
channel) can start a reception at any wall-clock moment. While such a sink
is attached, the blind fast-forward span is capped to a fraction of a
serial character time, so a byte landing mid-nap raises RBF and wakes the
CPU before a second byte can complete and overrun Paula's one-word receive
buffer -- matching a real machine, which wakes from `STOP` within
microseconds of the interrupt.

## Exceptions

If the guest triggers something unimplemented (an exotic custom-register
edge case, say), the CPU may halt with an `EXCEPTION`. This is non-fatal:
the window stays alive showing the last framebuffer, and the debugger can
inspect the halted state.
