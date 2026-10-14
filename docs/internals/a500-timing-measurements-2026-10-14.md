# A500 timing measurements and remaining work, 14 October 2026

## Scope and method

Measurements used OneROM on the owner's PAL A500: 512 KiB chip RAM, 512 KiB
slow RAM, ECS Agnus 8372A (raw VPOSR `$a000`/`$a001`), OCS Denise (raw ID
`$ffff`). OneROM Fire-40-A ran firmware 0.8.0, USB plugin 0.3.2 and host-control
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
| Native BLIT STOP IRQ, 512 trials | 0 IRQ-beam or saved-PC mismatches | 256 trials +2 CCK; 256 trials +4 CCK | 256 exact; 256 still +2 CCK |
| Copper-generated BLIT/COPER/SOFT STOP IRQ, 384 trials | 0 IRQ-beam or saved-PC mismatches | 0 | 0 |
| Line texture/data/BZERO sweep, 2,560 trials | Destination OR and three words: 0 mismatches; flag: 872 mismatches | Separate line-delay candidate, see below | Destination data remains exact; flag remains unresolved |

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
All 896 IRQ trials agree on the saved exception PC. Copper-generated sources
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
* STOP fast-forwarding is bounded by a BLIT request still pending after the
  sequencer finishes. Native BLIT recognition accounts for its source phase
  preceding INTREQR readback by three CCK; software/Copper writes and later
  unmasking retain their generic recognition paths. This improves the measured
  native IRQ residual but does not close the odd-phase case.
* Normal BZERO visibility is delayed three CCK after the mathematical
  accumulator clears; fill's extra idle phase (`USE-D && !USE-C`) adds one.
  An absolute Bus deadline continues after engine completion or DMA disabling.
  New starts reset it. Self-describing save states default the new field for
  older payloads and preserve an in-flight deadline; no format version changed.
  The one-CCK line candidate is still provisional and is not proof of the
  broader line flag behavior.
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

### 1. Close native BLIT STOP's remaining odd-phase +2 CCK

Priority: high. Reuse the synchronized kind-23 hardware capture before taking
more measurements. Trace E, INTREQ readback assertion, the modeled IPL deadline,
each STOP poll, recognition and the handler's first beam read. Check whether
`instructions_for_cck`/idle chunk conversion rounds a three-CCK deadline to
four CCK, followed by another poll-phase adjustment. Compare the ordinary
execution loop with STOP advancing one CCK at a time. Fix any scheduler
overshoot rather than adding a title-dependent delay or shifting all IRQs.

If the residual survives exact stepping, extend kind 23 with at least sixteen
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
stages if software observations remain ambiguous; it is not required to first
test the scheduler-rounding hypothesis.

### 2. Determine line-mode BZERO semantics before changing the flag

Priority: high. Kind 24 proves that hardware, WinUAE and Copperline draw the
same observed data, yet hardware clears BZERO even for an all-zero texture
whose complete 1,024-word destination OR is zero. Writing BLTBDAT before versus
after BLTCON1 makes no difference. USEB off and on alter the edge, but texture
positions 1/4/8 do not predict the hardware flag as they predict emulator
flags. The one-CCK propagation candidate has 956 mismatches in this broader
sweep, versus WinUAE's 872; this is not an accuracy improvement claim for line
mode. The final combined executable was not separately recaptured for kind
24; its line computation/deadline code is unchanged from that candidate.

Next build a reviewed kind-25 logic matrix: minterms `$00/$ff/$ca/$f0`, A data
zero/`$8000`, B texture zero/`$ffff`/`$1000`, C initialized zero/`$ffff`, USEB
off/on, and line lengths 1/2/22. Read at NOP delays 0/1/2/4/8/16 in both NOP
and EXG phases (3,456 rows). Capture full DMACONR and beam first, then after
completion the destination OR and words at `$10000/$10050/$100a0`. Explicitly
initialize masks, pointers, controls and modulo registers; never use
read-modify-write instructions on write-only custom registers. Verify matrix
bounds, assemble and run WinUAE preflight before flashing.

This distinguishes an intermediate line-controller/ALU contribution to
BZERO from the final stored pixel, a prior-pipeline latch, channel-enable
effects, or a propagation delay. Add SING, disabled A/C, and other minterms
only if these controls require them. Derive one hardware rule, then verify
both the new matrix and the normal/fill 4,608-row sweep. Do not make the flag
follow rendered pixels merely because that seems intuitive. The interrupted,
unvalidated kind-25 work is preserved locally in
`tmp/timing-isolation/unfinished-line-logic.zip`; it is excluded from commits.

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
| Line BZERO and data | `e9ca4607f72ec69f6c5728b87feb1398bb594f2748e995f6642981127ea8e019` | `rom-amiga500-bzero-line-data1` |

Executable SHA-256 values (invocation provenance, not a current-path guess):

* Pre-CPU release baseline: `be0b85e7a67145cc8861bbbc0eab7cb5a75e719ebd81a4e75942583fb423efbb`.
* Pre-blitter aligned candidate: `b6cdf62b220ccd5634cf9a92dd4d87e30d2d2dc26671d427aa35548f74ed2b36`.
* Corrected CPU/sequencer/sampling: `69f242e04cbc696d04dbc3c38d8b7a55e96fa053641234d0b51b43df772d786b`.
* BLTPRI/STOP-deadline candidate before BZERO/native-phase correction: `0e5b29f499ed69aae1ca3b17902194b22b0d7d6a81f1c5bf1a3d9da2b92e5bf2`.
* Initial BZERO candidate, broad fill delay: `f08a8568ac55ebdcb3428ac521db363fcfd3754a315338bdd357c4502a1296e6`.
* Final measured normal/fill/native-phase candidate: `0c8734718fc6cf9358f0e6ab3c4d3583ca75e2a3021fac456d5b70acc0f1540b`.
* WinUAE: `f3e55700fd811cd543fdfebf6c3221cfaa3d385828f110c6d3eb534c64503123`.

The final 4,608-row BZERO packet SHA equals hardware:
`3fc9ac2ef2134a5b03090fefca5d5251e5445c71fbee8c9c2f0759284138e7fe`.
IRQ hardware packet SHA is
`9c74959d803b60cea0f2823372d2b6e52a4dd896ac005b366846da7bd6c883c0`;
the final Copperline IRQ packet is
`dad7c224a3d6031333e5c68c60849f45802efd42ad05f8c884d99cf4145218cc`.
Line-data hardware SHA is
`d890ce92ae482a59f739e74a61b10b041fe70ac344277bbce4039da177b5bed5`.
Superseded bundles are preserved and explicitly excluded from claims.

## Final local validation

* Optimized `cargo test --profile ci --locked -j 2` completed successfully with
  two documented pre-existing Windows tests filtered: GPU picture composition
  tolerance (`picture_draws_match_the_composed_texture`) and the parallel-input
  global-state test (`numpad_mapping_stands_in_for_the_missing_gamepad`). The
  library result is 4,079 passed, zero failed, 43 ignored and two filtered.
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
* The probe repository's 34 Python tests pass. Only reviewed, assembled and
  captured experiment modes were committed; unfinished kind 25 was excluded.
* Only `timing-test/golden/timing-test.png` was refreshed after reviewing the
  numeric changes. All other screenshot references remain unchanged. These
  PNGs are emulator regression renders, not physical-machine video captures.

An unrelated, pre-existing Copperline process was accidentally stopped during
an earlier cleanup. Its running state could not be recovered. Subsequent
runner cleanup was restricted to each runner's own subprocess.
