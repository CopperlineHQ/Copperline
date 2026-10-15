# A500 timing measurements and remaining work, 14-15 October 2026

## Scope and method

Measurements used One ROM on the owner's PAL A500: 512 KiB chip RAM, 512 KiB
slow RAM, ECS Agnus 8372A (raw VPOSR `$a000`/`$a001`), OCS Denise (raw ID
`$ffff`). One ROM Fire-40-A ran firmware 0.8.0, USB plugin 0.3.2 and host-control
0.1.6, with SEL_C resetting the 68000. Timed observations were collected into
RAM before RBCP USB transfer. Captures validate their additive checksum,
source ID, trial descriptors, timeouts and skipped rows. No RBCP command ran
inside a timed kernel. ROMs, firmware and captures remain local assets.

WinUAE 6.0.3 used A500 Best Compatibility, PAL, ECS Agnus, a cycle-exact
68000, cycle-exact memory/blitter, matching memory sizes and no JIT. Copperline
used the same machine configuration in deterministic headless runs. ROM
kernels align horizontal launch phase and record the actual full launch beam;
the IRQ controls also rendezvous at line 64 before launching. Elapsed beam
time is `227 * deltaV + deltaH`, with unsafe line-edge/wrap samples excluded.
Same descriptor numbers alone are insufficient evidence of the same phase.

The original Kickstart was restored after the last hardware measurement. A
524,288-byte live-ROM read has SHA-256
`9797dfe08af1e48271528411a41eda20fb03ee3cf387e610ce50eedfb0c457eb`, identical
to the pre-measurement backup. No capture establishes timing for every OCS/ECS
revision, AGA, faster CPU, NTSC mode or arbitrary contention pattern.

## Comparison with hardware

The before columns identify the capture immediately before the relevant
correction, rather than pretending all experiments used one unchanged
baseline. Their executable hashes are recorded below. Counts are mismatches
against the physical machine, not against an emulator screenshot.

| Observation | WinUAE Best Compatibility | Copperline before correction | Copperline after correction |
| --- | --- | --- | --- |
| Aligned CPU/DIV sweep, 1,792 scheduled trials | 0 elapsed mismatches / 1,640 safe same-launch pairs | 655 / 1,685 safe same-launch pairs | 0 / 1,732 safe same-launch pairs |
| Busy observer, BLTPRI off, 3,456 trials | 0 state or beam-time mismatches | 26 busy-bit mismatches; 1,176 beam endpoints differ | 0 state or beam-time mismatches |
| BLIT request readback, BLTPRI off, 3,456 trials | 0 | 30 request-bit mismatches; 1,176 beam endpoints differ | 0 |
| Final destination read, BLTPRI off, 3,456 trials | 0 | 26 word mismatches; 1,176 beam endpoints differ | 0 |
| Busy/request/memory observers with BLTPRI on, 10,368 trials | 0 masked-state or elapsed mismatches | Startup CPU fence was disproved by the early read samples | 0 masked-state or elapsed mismatches |
| Fine BZERO/BBUSY/valid DMACONR sweep, 4,608 trials | 20 flag mismatches; 0 timing mismatches | 86 flag mismatches; 0 timing mismatches | 0; complete packet equals hardware byte-for-byte |
| Synchronized native and Copper STOP IRQ paths, 896 trials | 0 IRQ-beam or saved-PC mismatches | 256 native trials +2 CCK; remaining 640 paths exact | 0; result packet matches A500 byte-for-byte |
| Line-data/BZERO sweep, kind 24, 2,560 trials | Destination data 0; BZERO 872 mismatches | 956 BZERO mismatches at 1-CCK delay | 932 BZERO mismatches at 0-CCK extra delay |
| Line minterm/BZERO sweep, kind 25, 3,456 trials | 74 BZERO mismatches; 0 beam/data mismatches | 74 BZERO mismatches | 74 BZERO mismatches |
| Fine line BZERO sweep, kind 26, 544 trials | 434 BZERO mismatches; 0 beam/data mismatches | 454 BZERO mismatches | 450 BZERO mismatches |
| Line input/order sweep, kind 27, 3,360 trials | 624 BZERO mismatches; 0 beam/data mismatches | 652 BZERO mismatches | 596 BZERO mismatches |

Across both BLTPRI settings the three primary aligned observers provide
20,736 exact masked-state and elapsed-time matches for the corrected blitter
sequencer. Those masks intentionally observe BBUSY, BLIT request or memory;
they do not establish correctness of every other bit in DMACONR/INTREQR.
The additional BZERO sweep tests the defined DMACONR mask `$67ff` explicitly.

In the six-plane, 64-iteration aligned kernels, all three systems measure
normal DIVU at 4763–4767 CCK, worst-case DIVU at 5102, and DIVS at
5249–5257. These include shared loop and readback overhead. The old claim of
191 CCK early execution came from a defective beam conversion that doubled
HPOS, plus unmatched launch phases. It is superseded, not a remaining
191-clock CPU defect. The corrected legacy table's hardware 4891 versus
Copperline 4886 is also phase-unmatched and must not be treated as a five-clock
residual.

For the native 22-word/22-pixel STOP controls, hardware and WinUAE measure
83/84 CCK for D-only/AD copy, 105/106 for fill, and 127/128 for line mode.
Before the source-phase correction Copperline measures 86/87, 108/109 and
130/131. Afterwards it measures 84/85, 106/107 and 128/129. The NOP phase is
exact; the EXG phase remains +2. Results are identical with the same handler
in chip RAM or ROM, excluding handler fetch placement as the explanation.
All 896 IRQ trials agree on the saved exception PC. A later STOP fast-forward
fix preserves the nearest device-event deadline in colour clocks instead of
advancing the rounded instruction budget past it. With that fix, all 896
Copperline result records match the A500 packet byte-for-byte; WinUAE has zero
IRQ-beam and saved-PC mismatches as well. The older +2 CCK odd-phase residual
is closed for this synchronized kind-23 matrix. Copper-generated sources
measure 261/262 CCK from launch on all three systems.

## Changes made

* MC68000 DIVU/DIVS flush internal computation clocks before their final
  prefetch and IPL poll. MULU/MULS keep their distinct existing sequence.
  Separate CPU-core bus-sequence tests cover operands, addressing modes,
  overflow and multiplier controls.
* Normal and line blits have two internal startup clocks. BBUSY clears after
  E/the line tail, before the final destination flush. BLIT request readback
  is independently scheduled three clocks after E, even if the last D access
  stalls.
* BLTPRI follows actual bus requests. Startup and first-D bubbles are available
  to the CPU; the unsupported extra startup fence was removed. The public
  compatibility query remains present and returns false.
* A 68000 chip-memory transfer samples data at each granted slot before its
  trailing DMA clock. Byte, word, two-word long and the existing AGA wide-grant
  path have regressions. The accelerated CPU transfer path is unchanged.
* STOP fast-forwarding now retains the earliest device-event deadline in CCK
  as well as the rounded CPU instruction budget. Idle device advancement is
  clamped to that deadline, including clocks already advanced by the CPU bus.
  The exact 896-row One ROM IRQ capture verifies all native and Copper-generated
  cases, including the formerly +2 CCK odd phase.
* Normal BZERO visibility is delayed three CCK after the mathematical
  accumulator clears; fill's extra idle phase (`USE-D && !USE-C`) adds one.
  An absolute Bus deadline continues after engine completion or DMA disabling.
  New starts reset it. Self-describing save states default the new field for
  older payloads and preserve an in-flight deadline; no format version changed.
  The line-mode visibility delay was reduced from one extra CCK to zero. This
  lowers BZERO mismatches in three of four line sweeps and leaves normal/fill
  results byte-identical, but line BZERO still needs a rule for its remaining
  input-dependent residual.
* `timing-test` uses the corrected linear PAL beam metric. A two-byte,
  four-CPU-clock NOP replaces the old doubling instruction, preserving its
  size and timing cost. Its real committed binary is exercised by a regression.
  Screenshot references are emulator regression images, not hardware captures.
* Default builds use the existing versioned `vendor/m68k` submodule. Root and
  frontend lockfiles resolve that checkout. Only workflows compiling/resolving
  Copperline fetch the first-level submodule; recursive fixture checkout is
  unnecessary for Copperline builds. The independent cputest runner continues
  using its own registry dependency.

## Open work and measurement plan

### 1. Extend STOP coverage beyond the measured matrix

The scheduler-rounding issue is fixed for the synchronized kind-23 matrix.
Keeping the earliest device-event deadline in CCK eliminates the prior +2 CCK
odd-phase residual: all 896 Copperline records now match the A500 packet
byte-for-byte, including IRQ beam and saved exception PC. The follow-up below
extends phase and contention coverage beyond this exact test matrix.

For broader coverage, extend kind 23 with at least sixteen
source horizontal phases, 0–31 NOP positions before STOP and an EXG phase flip.
Keep native D-only, AD copy, descending fill and line controls, plus Copper
BLIT/COPER/SOFT using the same IRQ levels and byte-identical RAM/ROM handlers.
Add one-word/two-word sizes and deliberate final-D contention from bitplanes
and Copper. Record the primary INTREQR read separately from the handler beam;
do not let preliminary RAM reads disturb the timed observer. Acceptance:
identical launch, IRQ beam and saved PC for every unambiguous pair, with the
existing Copper 37/38-CCK parity tests and CIA STOP results preserved.

The three-CCK native source-phase adjustment is an inference from combined
paths, not a direct measurement of Paula's physical IPL pins. A logic analyzer
on BLIT/INTREQ-related signals and CPU IPL would distinguish those internal
stages if a later phase or contention sweep exposes a residual.

### 2. Finish line-mode BZERO isolation

The 15 October follow-up collected 25,636 validated new A500 records with
One ROM, including the kind-33 repeat. See [the detailed report and next-run
handoff](a500-line-bzero-2026-10-15.md) for source IDs, comparisons and the
current acceptance results. The old single-region destination observer did
not cover later D writes, which follow the C pointer in line mode; new
probes scan both regions. Limit earlier zero-data statements accordingly.

Full-minterm, nonuniform-C and length sweeps isolate a texture-counter-wrap
component using previous A/C holds. The narrowed correction applies only
with USEB clear; extending it to B DMA introduces measured false clears.
Other non-wrap effects remain: kind38 demonstrates a C-DMA-dependent flag
change even with LF=cc, which ignores C in the ordinary minterm.

Next, at BSH4, A=B=0, LF=cc and two pixels, vary one-hot C words separately
at the first and second C source addresses. Cross USEB/USEC and include
one-pixel controls with BLTCDAT held at zero. Then measure LF=c0 with all
A shifts to reconstruct the effective B-bit incidence without depending on
C in the minterm. Repeat rare opcode-control differences and prime all
normal-mode A/B/C holds if needed. Do not implement a BSH lookup table.

Kinds36–38 are now measured; invalid packaging drafts are excluded. The
fresh live-ROM backup was restored, hash-verified and reset released after
each capture. No test ROM remains running on hardware.

### 3. Finish regression and scope checks

Re-run all optimized unit/integration and 45 screenshot tests, refreshing only
the expected timing-test reference after reviewing its changed numeric table.
Keep screenshot generation distinct from hardware trace comparisons. Repeat
the measured observer sweeps if a change touches arbitration, sampling or the
blitter sequencer. Check save/resume across BZERO and IRQ deadlines. Extend
the phase matrix to bitplane/Copper/sprite/refresh contention, timer IRQs and
scanline/frame boundaries. Measure host performance on an identical
deterministic workload before claiming speed is unaffected; no controlled
performance benchmark was completed here. AGA/other CPU accuracy requires
appropriate hardware evidence rather than extrapolating this A500 result.

For an upstream submission, publish the m68k submodule commit before the
parent gitlink, check source-archive/Homebrew packaging (GitHub archives omit
submodule contents), and decide whether upstream accepts the path dependency
or needs a published CPU crate release. These commits are local; no push was
authorized or performed.

## Evidence inventory

Capture folders are under the sibling `a500-irq-probe/captures` directory.
Immutable ROM/source bundles are under Copperline's ignored
`tmp/timing-isolation`. The probe repository contains the builders, runner,
checksum parser and portable comparison tests. Its detailed report is
`docs/rom-timing-isolation-2026-10-14.md`.

| Bundle | Source ID | Hardware capture |
| --- | --- | --- |
| CPU aligned | `89893768d65920f312d7fd2b41fc81de847fe72c6ca884360e3c7e216d03190b` | `rom-amiga500-cpu-aligned1` |
| Busy, priority off | `fad8e342ac5f90057737969facd54d568d616502da9423d4d7b7a08cd5fa46b7` | `rom-amiga500-aligned-busy1` |
| IRQ, priority off | `ea4481eb8632945cd80d2a4fba4f45f770400401716884e73d79a91ee517c464` | `rom-amiga500-aligned-irq1` |
| Memory, priority off | `be743a9aa847ee58749678375b21b8fd5d72adfdef59d646c270726d29c27cd3` | `rom-amiga500-aligned-memory1` |
| BZERO fine edges | `634599ff1094acc5fd95f87f8ab34264a61a12407ff0577330b2b30f348fc076` | `rom-amiga500-bzero-edges1` |
| Synchronized IRQ paths | `e3ed3ab473e1a1f2c2f65d6ad03b33684cbf2eb7aa68f076fb7a75d4cd1a4e0a` | `rom-amiga500-irq-paths2` |
| Line BZERO and data, kind 24 | `e9ca4607f72ec69f6c5728b87feb1398bb594f2748e995f6642981127ea8e019` | `rom-amiga500-bzero-line-data1` |
| Line logic, kind 25 | `31b9aa0cf37222a2b6dca7c1d7cc982beed7543a3012d43a26c9fe1a6a33eaa7` | `rom-amiga500-line-logic-corrected25` |
| Fine line BZERO, kind 26 | `c340ff9336275a570317fc8f0b83a6ee5d0edb6a034a8365f74edfd8f6f62bd0` | `rom-amiga500-line-logic-fine26` |
| Line inputs and ordering, kind 27 | `c610b7ff62314870a21a03dfab5aae8c1ff87ce01578f57c7a2e9a42f6476223` | `rom-amiga500-line-logic-followup27` |

Executable SHA-256 values (invocation provenance, not a current-path guess):

* Pre-CPU release baseline: `be0b85e7a67145cc8861bbbc0eab7cb5a75e719ebd81a4e75942583fb423efbb`.
* Pre-blitter aligned candidate: `b6cdf62b220ccd5634cf9a92dd4d87e30d2d2dc26671d427aa35548f74ed2b36`.
* Corrected CPU/sequencer/sampling: `69f242e04cbc696d04dbc3c38d8b7a55e96fa053641234d0b51b43df772d786b`.
* BLTPRI/STOP-deadline candidate before BZERO/native-phase correction: `0e5b29f499ed69aae1ca3b17902194b22b0d7d6a81f1c5bf1a3d9da2b92e5bf2`.
* Initial BZERO candidate, broad fill delay: `f08a8568ac55ebdcb3428ac521db363fcfd3754a315338bdd357c4502a1296e6`.
* Final measured normal/fill/native-phase candidate: `0c8734718fc6cf9358f0e6ab3c4d3583ca75e2a3021fac456d5b70acc0f1540b`.
* STOP deadline plus line-mode BZERO candidate measured against kinds 22-27: `97a0b51616eb47bdfd060cb8bd66a2a53ad1ca1b1c4e9242c9858ccf5561dc38`.
* WinUAE: `f3e55700fd811cd543fdfebf6c3221cfaa3d385828f110c6d3eb534c64503123`.

The final 4,608-row BZERO packet SHA equals hardware:
`3fc9ac2ef2134a5b03090fefca5d5251e5445c71fbee8c9c2f0759284138e7fe`.
IRQ hardware packet SHA is
`9c74959d803b60cea0f2823372d2b6e52a4dd896ac005b366846da7bd6c883c0`;
the STOP-deadline Copperline IRQ packet is the same SHA, byte-for-byte.
Line-data hardware SHA is
`d890ce92ae482a59f739e74a61b10b041fe70ac344277bbce4039da177b5bed5`.
Superseded bundles are preserved and explicitly excluded from claims.

## Final local validation

* Optimized `cargo test --profile ci --locked -j 2` completed successfully with
  two documented pre-existing Windows tests filtered: GPU picture composition
  tolerance (`picture_draws_match_the_composed_texture`) and the parallel-input
  global-state test (`numpad_mapping_stands_in_for_the_missing_gamepad`). The
  library result is 4,081 passed, zero failed, 43 ignored and two filtered.
  Non-ignored integration tests, including the real beam-metric binary test
  and all 45 golden screenshots, pass. Asset-gated ignored tests were not run.
* Root `cargo fmt --all -- --check` and standard
  `cargo clippy --all-targets --locked` pass. Clippy retains six pre-existing
  Windows warnings: one unused test import, three unused socket constants,
  and two test closures. This is not a claim that strict root `-D warnings`
  passes or that every feature/target was built locally.
* m68k passes all CONTRIBUTING checks: formatting, strict all-target/all-feature
  Clippy, all-feature tests and clean-tree `cargo package --locked` after its
  local commit. The CPU commit is `54b8623`.
* The probe repository's 40 Python tests pass. Only reviewed, assembled and
  captured experiment modes were committed; unfinished kind 25 was excluded.
* Only `timing-test/golden/timing-test.png` was refreshed after reviewing the
  numeric changes. All other screenshot references remain unchanged. These
  PNGs are emulator regression renders, not physical-machine video captures.

An unrelated, pre-existing Copperline process was accidentally stopped during
an earlier cleanup. Its running state could not be recovered. Subsequent
runner cleanup was restricted to each runner's own subprocess.
