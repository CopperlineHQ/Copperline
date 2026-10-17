# A500 Copper launch and line BZERO follow-up

Measurements use a PAL A500 with 512 KiB chip and 512 KiB slow RAM, ECS Agnus
and OCS Denise, with One ROM live ROM replacement and checksum-protected USB
result collection. Each run backs up the current live ROM, verifies the test
image after writing it, and restores and verifies that run's backup before
releasing reset. ROM images and raw captures remain local assets.

Toni Wilen's EAB observations motivated the Copper-start and register-handoff
controls: [post 1426048](https://eab.abime.net/showpost.php?p=1426048) describes
possible Copper/blitter start inhibition, and
[post 1422393](https://eab.abime.net/showpost.php?p=1422393) describes register
updates after BBUSY clears while the last destination write is pending.
These are hypotheses to test, not substitutes for measurements.

## Provenance

Builders and immutable archives are in the sibling `amiga-hardware-probes`
repository. Each archive contains the exact assembler inputs and descriptors;
their source fingerprints were independently recomputed before interpretation.

| Suite | Source fingerprint | Trials |
| --- | --- | ---: |
| Copper WAIT, branch-free CPU | `b5549e3f000ca8e0eb973d67f04752b2c66c82aab432133073296d1160fdb524` | 1,024 |
| Same list, BLTSIZE replaced by NOOP | `59b0df645e65578d261ce8b65404afedb8f78ee4c21bd417bccdce1a937c8b43` | 1,024 |
| CPU BBUSY/register handoff | `836cfdc91ac3201e47f18c0770ddffc4e32be32054d215cec491b38cfb46d151` | 2,304 |
| Line A-data one-hot controls | `9a505b99b448c4fa93d69b644ebd72cc25191382417e591b0612c962dc210bf5` | 1,536 |

The Copperline baseline executable SHA-256 is
`f1111a474bb1ffe384a64a03226a5377be2af912aa9b63bf2e070f2cff5c78ce`.
The WinUAE executable SHA-256 is
`1060375cbfa4bbcde76e4ad2cd0876fca2f675a997ed33a51937ea678a2cbd09`.
WinUAE uses real-speed 68000, compatible CPU, CPU/memory/blitter cycle exact,
PAL ECS Agnus, no JIT, and matching RAM sizes. Its resolved configuration is
saved with each run. Copperline uses the matching ECS Agnus/OCS Denise config.

## Copper WAIT controls

The Copper starts a 16-word D-only or A-to-D blit and executes one or two
already-eligible BFD-clear WAITs before requesting COPER. The CPU executes
ROM NOPs or chip-memory reads without a loop branch or DMACONR polling.
The IRQ handler's first VHPOSR read is an observation of the complete path,
not a direct timestamp of the Copper WAIT edge.

All 1,024 rows complete successfully on each platform with matching marker
count, destination data and BBUSY-clear readback. Excluding horizontal
positions 0/1, which straddle VHPOSR's pipelined line wrap, and requiring equal
launch timestamps gives:

| Comparison against A500 | Matched rows | IRQ observation difference |
| --- | ---: | --- |
| Baseline Copperline, blit present | 218 | median -2 colour clocks |
| WinUAE, blit present | 246 | all exact |
| Baseline Copperline, no-blit control | 307 | all exact |
| WinUAE, no-blit control | 255 | all exact |

Requiring equal saved PCs as well leaves 150 Copperline blit rows with median
-2 clocks and 154 no-blit rows with exact timestamps. Removing DBRA did not
remove the residual. This isolates a blit-dependent path; it does not justify
adding a blanket IRQ delay or establish the reported start-inhibition bug.

Captures: `rom-amiga500-copper-wait-unrolled-run1/capture`,
`rom-amiga500-copper-wait-no-blit-control/capture`, and corresponding
`rom-copperline-*` / `rom-winuae-*` folders. The original loop control was also
repeated on A500; 1,023/1,024 records were identical between runs.

A candidate preventing a newly Copper-started blit's internal startup step
from executing in the MOVE's own quantum passed two focused ordering tests,
but produced byte-identical blit and no-blit result packets. It was removed
from the working code rather than presented as a timing fix. Its optimized
executable SHA-256 is
`b287df4d9c23d7dd762d4758227593f673a5ae34de92246fcc95b72a7b4da4dd`.
The CPU-launch control then starts the blit with the Copper already waiting.
Its first form stores a VHPOSR sample to RAM before BLTSIZE, so it cannot
establish the exact write phase. The revised finish sweep keeps that sample
in a register and writes BLTSIZE in the next instruction. It varies widths
1/4/8/16, one/two WAITs, CPU ROM NOPs/chip reads, BLTPRI, launch padding and
phase, without display DMA (2,048 trials). Its source fingerprint is
`2f6f69331db2e4c964d333f0aab48c5054194f6eaec4a9cef658b7fc59106c7a`.

All revised-sweep rows complete with expected destination words and the IRQ
after CPU BLTSIZE. Requiring equal launch observations gives 374 baseline
Copperline comparisons: 326 exact, 39 two clocks late and nine eight clocks
late. The one-word controls are all exact; most differences occur with
8/16-word blits. WinUAE matches all 396 rows whose launch observations match
hardware. Pairwise populations differ because actual launch phases differ.
These CPU-launch residuals have the opposite sign to the Copper-launch
median; a universal start or IRQ delay would be unjustified.

Captures: `rom-amiga500-copper-wait-finish-sweep-v2-run1/capture` and the
corresponding Copperline/WinUAE folders. The earlier `finish-sweep` archive
without `v2` is excluded: its late launch gate allowed the Copper request to
precede CPU BLTSIZE in some rows.

Next measure BBUSY directly around Copper BLTSIZE with one status read per
phase, followed by an adjacent-read control. Record actual beam brackets and
include matched no-blit lists. CPU custom-register reads can themselves
perturb bus arbitration, so an eight-read train alone cannot establish an
unobserved launch edge. Separate onset, completion, Copper wake and CPU IRQ
observation before changing any of those delays.

## CPU register-handoff controls

All 2,304 A500 records complete without timeout. The observed immediate last-D
read and both settled destination endpoints equal the expected FFFF (D-only
and ABCD) or AAAA (A-to-D) on all three platforms. No stale final word is
observed by this polling sequence. This confirms the tested update sequence;
it does not prove that software reads the pending-write interval itself.

Launch beam timestamps match all 2,304 descriptors across the three platforms.
With extra display/Copper DMA disabled, all 576 handoff beam observations also
match A500 exactly. Four/six-plane contention exposes further Copperline
timing differences. WinUAE matches every clean four-plane and six-plane
handoff timestamp; the six-plane-plus-Copper-burst workload has 44 nonmatching
clean observations out of 548. It therefore must not be treated as hardware
truth for all contention cases. The handoff timestamp is measured after
polling, a memory read and BLTCON0 write; poll-loop quantization can magnify a
small preceding difference.

This kind repurposes raw result offsets: +14 first-clear DMACONR, +16 immediate
last-D read, +18 postwrite DMACONR, +20/+22 handoff beam reads, +24/+26 final
first/last D, +28 INTREQR, +30 settled beam, +32 status. Generic CSV column
names such as `counter`, `icr` or `saved_pc` do not describe those fields.

Capture: `rom-amiga500-blitter-handoff-kind49-run1/capture`; packet SHA-256
`480f5177aea788d870b4714d49d6b6465a9e9f8139e6c75941417cb4aa7a0b3b`.

## Line BZERO one-hot controls

All 1,536 records have identical launch/read/end beam observations and final
destination words on A500, baseline Copperline and WinUAE. Eight repeats of
each case are identical. Nonzero-B controls verify the effective A word:
the first pixel writes `ADAT >> ASHIFT`, and the second writes the next bit
position. Zero-B controls write zero to both destinations.

Only the two-pixel, zero-B wrap cases differ. With target `n` selecting
`ADAT = 0x8000 >> n`, A500 leaves BZERO set for:

| ASHIFT | A500 targets with BZERO set | Baseline Copperline targets |
| ---: | --- | --- |
| 0 | 2, 4 | none |
| 2 | 0, 2, 14, 15 | 14, 15 |
| 4 | 0, 12, 13, 14, 15 | 12, 13, 14, 15 |

Copperline differs in 40 records, WinUAE in 296; differences are confined to
the eight DMACONR samples' BZERO bit. BBUSY and destination words match.
The unexplained exceptions correspond to effective A positions 13 and 11.
Do not encode that incidence map as an unexplained constant. Next controls
must distinguish intermediate B/C inputs, minterm evaluation and flag
reduction using other minterms, multi-bit A inputs and non-wrap B shifts.

Capture: `rom-amiga500-line-a-bit-kind48-run1/capture`; packet SHA-256
`d56881c4b62fd708ec94e71adccb862a35814eb9185621dc702ef6605f631961`.

### Minterm and non-wrap controls

The 2,048-row truth-control archive has source fingerprint
`feb59528cda4d4797efdb5776d9d385b6d34948360074698c5748b892f489621`.
It crosses eight A patterns, minterms 00/FF/F0/CC/33/AA/40/CA, zero/all-one
register B data, B shifts 0/1 and eight repeats. C DMA remains enabled from
zeroed memory; B DMA is disabled. The original observer is unchanged.

Every beam observation and both destination words match on all three
platforms, and every repeat is identical. At BSHIFT=0 with B=0, hardware
sets BZERO for minterms 40 and CA only when A is 2000, 0800 or 2800; adding
bit 0 (A=2801) clears it. Direct-A minterm F0 clears BZERO for all nonzero A,
so the hardware does detect those bits in the ordinary pixel calculation.
Minterm CC clears BZERO on hardware, as in Copperline; WinUAE keeps it set.
At BSHIFT=1, the disputed zero-output cases keep BZERO set on all platforms.
These controls narrow the exception to the inter-pixel wrap evaluation.
They still do not independently measure the intermediate B word or establish
why the effective A-mask has these exceptions.

Capture: `rom-amiga500-line-a-bit-truth-control-run1/capture`; packet SHA-256
`d767ef553a221a0ace394153d993fb59f1cc0369a11a43f292d4c20d91c20304`.
For both kind48 modes, raw offsets +20..+34 are eight DMACONR words, including
offset +32; the generic decoder's `status` field is therefore a flag sample,
not a timeout status. Completion is verified by the packet's complete flag,
checksum and descriptor count.

## Retained B data and shifter carry

Separate history controls prime B through CPU writes, normal B DMA, or line
B DMA, then run a target with USEB clear. They distinguish raw BLTBDAT from
the B hold consumed by a normal-mode minterm. Target writes to BLTBDAT are
made before clearing CON1, after clearing it, or omitted. The preceding line
can therefore expose both its retained hold and the raw-word carry used by
a later CPU write.

| Suite | Source fingerprint | Trials |
| --- | --- | ---: |
| Line target after B-history primes | `e0a655764065fbc12c45fe5d41e438f4aa90f36c18d58f5f79135cf02947d9e9` | 2,016 |
| Normal target after B-history primes | `dc7c414cfc945bb7151027e08395b690b7836b8a1950cd77693454c57b2720aa` | 168 |
| Normal target after line with every DMA channel disabled | `7b12bf38b532443a30f23d89e1e6567300c0efbb724a0eb7ba614d21e5734425` | 48 |
| Held-out patterns, shifts and lengths | `f721cceba127d7b3f226ab1ac3ae8ae6ee458e5e33ae5ddada142c94204949d2` | 1,152 |

The held-out matrix crosses raw B words 0001/8000/A55A/1248, initial BSHIFT
0/1/7/15, one through three line pixels, three target write orders and eight
repeats. All hardware repeats are identical. With B DMA disabled, one pixel
retains `raw >> initial_BSHIFT`, while two/three pixels retain
`ROR(raw, (initial_BSHIFT - (pixels - 1)) & 15)`. A subsequent zero BLTBDAT
write before clearing CON1 uses raw as the previous barrel-shifter input at
`(initial_BSHIFT - pixels) & 15`; writing zero after clearing CON1 yields zero.
These statements describe the tested line state, not arbitrary writes during
an active line.

The first candidate corrected most outputs but failed seven one-pixel tuples
(56 repeated records): it incorrectly retained circularly wrapped bits in
the first pixel. The normal-history control separately exposed 16 remaining
records where line B DMA must preserve carry for a later CPU BLTBDAT write.
Both checks were necessary before accepting the final model.

The C-DMA-enabled cross-check repeats all 1,152 tuples with a zero C source
at $14000 and the first D sink at $15000 (subsequent D pointers follow C).
Every retained first-D result agrees with the DMA-disabled hardware cohort;
every second-D result is zero. Source fingerprint:
`1b1847727efe84edb602a69fc9f78d6e01f9caa7fa38321549875b83173aa327`;
hardware packet SHA-256:
`4decda1716cd77cf4c2943f3e80fe2f758472bbc4a7186a52fb78099a9e2b73a`.
The fresh live ROM was restored byte-for-byte and reset released successfully.

Raw B DMA data and the shifted hold must be published at fetch/pixel events,
rather than copied back indiscriminately on completion: a CPU write after
the final fetch must survive terminal drain. No additional timing delay or
serialized state field is needed. This data-retention correction does not
resolve the independent inter-pixel BZERO or Copper WAIT timing residuals.

### Validation and next-run handoff

The refined implementation passes all 35 focused scheduled-operation tests,
including repeated B fetches, the one-pixel shift, write-order matrix and
line-DMA carry. The first optimized candidate, before the held-out refinements,
reproduced the baseline one-hot, minterm, Copper WAIT and CPU finish-sweep
packets byte-for-byte. It reduced line-history destination mismatches from
144 to zero, but still had the 56/16 held-out defects described above. These
are intermediate-candidate results, not final executable validation.

At the user's time-limit wrap-up, the full optimized library suite and then
the refined optimized executable build were still running in local shell
session 99510. The library log is
`tmp/b-data-candidate-20261017/library-tests.log`; two previously documented
GUI tests are excluded, not repaired by this change. Strict Clippy, the full
integration/golden suite and a release build have not yet been rerun for this
commit. The previous baseline passed those checks as recorded in
`tmp/rebase-v1-validation-20261016.md` (with its documented GUI limitations).

Next run, in order:

1. Collect session 99510's outcome. Copy the refined `target/ci/copperline.exe`
   to a new immutable directory and record its SHA-256. The existing
   `tmp/b-data-candidate-20261017/copperline.exe` is the **intermediate** build.
2. Replay the four B-history/hold archives plus the new C-DMA cohort. Require
   zero destination mismatches, and report BZERO separately using the probe
   repository's typed `tools/compare_b_hold.py`. Replay the earlier one-hot,
   minterm and Copper timing controls to check for unrelated changes.
3. Run formatting, strict all-target/all-feature Clippy, integration/golden
   checks and the release build with `-j1`; report existing failures honestly.
4. Analyze the new kind50 direct Copper BBUSY pilot, source
   `5b588a7182bb8dcbbe84a3f52ab01d558031e5937be5346c24f49aa534e05237`.
   Baseline Copperline and WinUAE captures are complete; the A500 capture is
   `rom-amiga500-copper-start-busy-kind50-run1`. Verify its restoration report
   and packet before interpretation. Compare only matching actual beam
   brackets, distinguish single reads from eight-read trains, and preserve
   the no-blit controls. CPU reads can perturb onset; gate requests are not
   timestamps. Separate BBUSY onset/completion from Copper wake and IRQ entry.
5. Isolate the remaining BZERO-only wrap exceptions with independent C words,
   minterms and A-bit combinations. Do not introduce an unexplained A-bit
   mask or fixed IRQ delay to fit the observed rows.

Wrap-up update: kind50 collection completed with all 528 records. Its packet
SHA-256 is
`270476765cff39dd28f97ee08d3ed976da8f2f8a8683c32b7ea3050061decb4d`.
Fresh-ROM restoration and reset release are verified, with no errors.
Timing interpretation remains pending. The probe tools and handoff are
committed in `amiga-hardware-probes` at `30989b2`; its 139 Python tests pass.
The Copperline implementation is committed at `d66432b2`.

## Scope qualification

The MHO98 accepts 50M acquisition depth at 2 ms/div and exports 12.5M digital
samples, about 25 ms at a CPU-clock-derived sample interval near 2 ns. The
RG03 LA time-increment header still reports 0.5 ns and cannot be trusted.
This time calibration uses nominal PAL CPU frequency, not an independent
frequency reference. Some deep captures have no qualified custom accesses or
the selected trigger edge and are excluded from timing evidence. One overlaps
the live-ROM helper's startup/reset interval; a later normal Kickstart deep
capture also lacks the selected edge, so reset does not explain every case.
A subsequent normal Kickstart capture has varied address/control signals
and REGEN activity. Require actual qualified bus windows before interpreting
any future scope recording; a reported STOP state alone is insufficient.
